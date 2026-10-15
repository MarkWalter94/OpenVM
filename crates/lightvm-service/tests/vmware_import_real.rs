//! Import of real VMware VMs through the service (ignored test, ADR-0030,
//! R6-01 required action "real VMware VMs"): each `.vmx` of
//! `LIGHTVM_E2E_VMX` (list separated like `PATH`) is previewed, imported
//! with the real `qemu-img` (the VMware folder is read-only, hash-compared
//! before and after), started under the host accelerator, watched on the
//! console until the guest reaches a session entry — a desktop or a
//! graphical login screen, or a stable text login prompt for a non-Windows
//! guest — or a stop error screen (or the limit is reached), and stopped.
//!
//! The infrastructure is asserted (preview fields, import, converted
//! content identical to the source, source unchanged, no orphans, nothing
//! left, audit). The guest outcome (what the console showed) is recorded
//! in `LIGHTVM_VM_EVIDENCE_DIR` (PNG every 30 s and
//! `vmware-real-results.txt`); a session entry is asserted only for the
//! names listed in `LIGHTVM_VM_EXPECT_DESKTOP`, and refusal is asserted
//! only for the names
//! listed in `LIGHTVM_VM_EXPECT_REFUSED` (a real VM the importer must not
//! take, for example an unclean shutdown).
//!
//! Requires `LIGHTVM_E2E_QEMU_DIR`; optional `LIGHTVM_E2E_VMX` (required),
//! `LIGHTVM_E2E_HELPER_DIR` (folder of the approved storage helper,
//! `packaging/storage-helper`: without it a Windows guest moved to another
//! controller is imported for the manual preparation, as usual),
//! `LIGHTVM_VM_EVIDENCE_DIR` (default `lab/logs/vmware-real`),
//! `LIGHTVM_VM_DESKTOP_MIN` (default 10), `LIGHTVM_VM_EXPECT_DESKTOP`,
//! `LIGHTVM_VM_EXPECT_REFUSED`.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use lightvm_proto::ipc::{Request, ResponseBody, Role, ServerMessage, VmSummary};
use lightvm_proto::{
    AuditAction, AuditEvent, AuditOutcome, DisplayAdapter, GuestOs, OperationId, OperationStatus,
    ReplyError, VmAction, VmId, VmName, VmRequest, VmState,
};
use lightvm_service::adapters::audit::FileAuditSink;
use lightvm_service::adapters::backend::{QemuSetup, probe};
use lightvm_service::adapters::catalog::{ManagedMediaInspector, SqliteStore};
use lightvm_service::client::Client;
use lightvm_service::engine::{CoreEngine, core_config};
use lightvm_service::test_support::temp_dir;
use lightvm_service::{ServiceConfig, start};

const T: Duration = Duration::from_secs(120);

fn qemu_dir() -> PathBuf {
    PathBuf::from(std::env::var_os("LIGHTVM_E2E_QEMU_DIR").expect("LIGHTVM_E2E_QEMU_DIR not set"))
}

fn exe(n: &str) -> String {
    if cfg!(windows) {
        format!("{n}.exe")
    } else {
        n.to_owned()
    }
}

/// Path as the system resolves it (no `\\?\`), as the import requires.
fn resolved(p: &Path) -> PathBuf {
    let c = std::fs::canonicalize(p).unwrap();
    let s = c.to_string_lossy().into_owned();
    PathBuf::from(s.strip_prefix(r"\\?\").unwrap_or(&s))
}

fn short_dir(prefix: &str) -> PathBuf {
    let mut b = [0u8; 4];
    getrandom::fill(&mut b).unwrap();
    let d = std::env::temp_dir().join(format!(
        "{prefix}{:02x}{:02x}{:02x}{:02x}",
        b[0], b[1], b[2], b[3]
    ));
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// Removes a test catalog even if the test fails (read-only managed copies).
struct RemoveCatalog(PathBuf);

impl Drop for RemoveCatalog {
    fn drop(&mut self) {
        for _ in 0..60 {
            clear_readonly(&self.0);
            if std::fs::remove_dir_all(&self.0).is_ok() || !self.0.exists() {
                return;
            }
            std::thread::sleep(Duration::from_millis(500));
        }
        eprintln!("test catalog not removed: {}", self.0.display());
    }
}

fn clear_readonly(p: &Path) {
    if let Ok(rd) = std::fs::read_dir(p) {
        for e in rd.flatten() {
            let path = e.path();
            if path.is_dir() {
                clear_readonly(&path);
            } else if let Ok(m) = e.metadata() {
                let mut perm = m.permissions();
                #[allow(clippy::permissions_set_readonly_false)]
                perm.set_readonly(false);
                let _ = std::fs::set_permissions(&path, perm);
            }
        }
    }
}

async fn req(c: &mut Client, r: Request) -> ServerMessage {
    tokio::time::timeout(Duration::from_secs(600), c.request(r))
        .await
        .expect("timeout")
        .unwrap()
}

async fn submit(c: &mut Client, vm_id: VmId, action: VmAction) -> ServerMessage {
    req(
        c,
        Request::SubmitVm {
            request: VmRequest {
                operation_id: OperationId::new_v7(),
                vm_id,
                expected_run_id: None,
                action,
            },
        },
    )
    .await
}

fn op_ok(m: &ServerMessage) -> bool {
    matches!(
        m,
        ServerMessage::Response {
            result: ResponseBody::Operation {
                operation: Some(op),
                ..
            },
            ..
        } if matches!(op.status, OperationStatus::Succeeded | OperationStatus::Forwarded)
    )
}

async fn status(c: &mut Client, vm_id: VmId) -> VmSummary {
    match req(c, Request::VmStatus { vm_id }).await {
        ServerMessage::Response {
            result: ResponseBody::Vm { vm },
            ..
        } => vm,
        other => panic!("{other:?}"),
    }
}

async fn wait_state(c: &mut Client, vm_id: VmId, want: VmState, limit: Duration) -> bool {
    let t0 = Instant::now();
    while t0.elapsed() < limit {
        if status(c, vm_id).await.state == want {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    false
}

// ----- console framebuffer -------------------------------------------------

#[derive(Default)]
struct Screen {
    width: usize,
    height: usize,
    rgb: Vec<u8>,
    ended: Option<String>,
}

fn watch(ticket: &lightvm_proto::ipc::ConsoleTicket) -> Arc<Mutex<Screen>> {
    let seen = Arc::new(Mutex::new(Screen::default()));
    let s2 = seen.clone();
    let transport = rfb_client::Transport::Ticket {
        path: PathBuf::from(&ticket.socket_path),
        imp: rfb_client::UnixImpl::Socket2,
        token: ticket.token.clone(),
    };
    std::thread::spawn(move || {
        let mut conn = match rfb_client::Connection::connect(&transport) {
            Ok(c) => c,
            Err(e) => {
                s2.lock().unwrap().ended = Some(format!("connect: {e}"));
                return;
            }
        };
        loop {
            let r = conn.request_update().and_then(|()| conn.read_update());
            let mut s = s2.lock().unwrap();
            match r {
                Ok(_) => {
                    let fb = conn.framebuffer();
                    s.width = usize::from(fb.width());
                    s.height = usize::from(fb.height());
                    s.rgb = fb.to_rgb();
                }
                Err(e) => {
                    s.ended = Some(e.to_string());
                    return;
                }
            }
            drop(s);
            std::thread::sleep(Duration::from_millis(250));
        }
    });
    seen
}

fn snap(s: &Arc<Mutex<Screen>>) -> (usize, usize, Vec<u8>) {
    let s = s.lock().unwrap();
    (s.width, s.height, s.rgb.clone())
}

/// Lit pixels (any channel above 16) of the frame: the total, the frame
/// size in pixels, and the lit pixels in the top quarter of the frame. A
/// getty login draws its banner and prompt at the top of the screen, while
/// a bare boot cursor and scrolling kernel messages are at the bottom. A
/// text login prompt is a few hundred pixels of a 1280x800 frame (under
/// one thousandth: never express these counts in permille, the integer
/// division truncates the signal away).
fn lit_pixels(rgb: &[u8], width: usize) -> (u64, u64, u64) {
    let px = rgb.as_chunks::<3>().0;
    let n = px.len() as u64;
    let per_row = width.max(1) as u64;
    let top_rows = n / per_row / 4;
    let mut lit = 0;
    let mut top_lit = 0;
    for (i, p) in px.iter().enumerate() {
        if p.iter().any(|&c| c > 16) {
            lit += 1;
            if i as u64 / per_row < top_rows {
                top_lit += 1;
            }
        }
    }
    (lit, n, top_lit)
}

/// Changed bytes between two frames: a static screen with a blinking
/// cursor changes by a few hundred bytes, scrolling boot text by
/// thousands.
fn changed_bytes(a: &[u8], b: &[u8]) -> u64 {
    a.iter().zip(b).filter(|(x, y)| x != y).count() as u64
}

/// Distinct colours of the frame: a stop error screen or a text screen has a
/// handful (background plus text), a graphical desktop or login screen
/// dozens (icons, anti-aliased text, gradients).
fn distinct_colours(rgb: &[u8]) -> usize {
    rgb.as_chunks::<3>()
        .0
        .iter()
        .collect::<std::collections::HashSet<_>>()
        .len()
}

fn save_png(s: &Arc<Mutex<Screen>>, path: &Path) {
    let (w, h, rgb) = snap(s);
    if w == 0 {
        return;
    }
    let img = qemu_backend::screenshot::RgbImage {
        width: w as u32,
        height: h as u32,
        data: rgb,
    };
    if let Ok(png) = qemu_backend::screenshot::encode_png(&img) {
        let _ = std::fs::write(path, png);
    }
}

/// What the console showed when the wait ended.
#[derive(Clone, Copy, Debug)]
enum Reached {
    /// A terminal or login prompt: a small share of lit pixels in few
    /// colours, stable for a while (the guest booted to session entry).
    Text,
    /// A mostly lit frame with many colours: a desktop or a graphical
    /// login screen.
    Graphical,
    /// A mostly lit frame in a single colour: a stop error screen.
    Stop,
}

/// The class of a frame by its statistics. The wait stops at `Graphical`
/// (a desktop or a graphical login: many colours and a large lit share),
/// at `Stop` (a stop error screen: mostly lit in a single colour) and, for
/// a non-Windows guest, at `TextLike` when it stays like that for a while
/// (a getty login prompt: a small amount of text in few colours at the
/// top of the screen, static except the blinking cursor).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FrameClass {
    Nothing,
    TextLike,
    Graphical,
    Stop,
}

fn classify_frame(
    lit: u64,
    n: u64,
    top_lit: u64,
    colours: usize,
    min_colours: usize,
) -> FrameClass {
    if n == 0 {
        return FrameClass::Nothing;
    }
    if lit * 5 >= n * 2 && colours >= min_colours {
        return FrameClass::Graphical;
    }
    if lit * 10 >= 7 * n && colours < min_colours {
        return FrameClass::Stop;
    }
    if colours <= 8 && lit >= 32 && lit * 8 < n && top_lit * 2 >= lit {
        return FrameClass::TextLike;
    }
    FrameClass::Nothing
}

#[test]
fn frame_classifier_on_observed_frames() {
    // A 1280x800 frame (the observed console size), n pixels in total.
    let n: u64 = 1_024_000;
    // The Debian 12 getty login as measured on the 2026-10-14 run:
    // 704 grey (170,170,170) pixels of text in the top rows, 2 colours.
    assert_eq!(classify_frame(704, n, 704, 2, 16), FrameClass::TextLike);
    // A bare boot cursor: 64 lit pixels at the bottom of the screen.
    assert_eq!(classify_frame(64, n, 0, 2, 16), FrameClass::Nothing);
    // A BIOS setup prompt: a line of text in the middle of the screen.
    assert_eq!(classify_frame(1_500, n, 0, 3, 16), FrameClass::Nothing);
    // Scrolling kernel messages: a few thousand lit pixels at the bottom.
    assert_eq!(classify_frame(5_000, n, 0, 4, 16), FrameClass::Nothing);
    // A terminal full of log lines: too much text, not concentrated at the
    // top — recorded, not a session entry.
    assert_eq!(
        classify_frame(50_000, n, 12_500, 3, 16),
        FrameClass::Nothing
    );
    // A stop error screen (BSOD): almost the whole frame, a few colours.
    assert_eq!(classify_frame(970_000, n, 200_000, 4, 16), FrameClass::Stop);
    // A Windows boot logo on black: a small bright area, a handful of
    // anti-aliased colours — not a desktop, not a stop.
    assert_eq!(
        classify_frame(30_000, n, 10_000, 20, 16),
        FrameClass::Nothing
    );
    // A graphical login with a wallpaper: most of the frame lit.
    assert_eq!(
        classify_frame(700_000, n, 150_000, 200, 16),
        FrameClass::Graphical
    );
    // A graphical login on a dark background: still a session entry.
    assert_eq!(
        classify_frame(450_000, n, 100_000, 80, 16),
        FrameClass::Graphical
    );
    // A black frame: nothing.
    assert_eq!(classify_frame(0, n, 0, 1, 16), FrameClass::Nothing);
}

/// Watches the console until the guest reaches a session entry — a desktop
/// or a graphical login, or, for a non-Windows guest, a stable text login
/// prompt — or a stop error screen, or the limit. A frame with many
/// colours and a large lit share is a desktop or a graphical login; a
/// mostly lit frame in a single colour is a stop screen; a text login
/// prompt is a small amount of text in few colours that stays still
/// (scrolling boot messages change the frame, a getty prompt only blinks
/// its cursor). For a Windows guest a text frame is not a session entry
/// (the boot text of the guest OS looks the same) and is only recorded.
/// Saves a PNG every 30 s on the way.
async fn wait_reached(
    s: &Arc<Mutex<Screen>>,
    limit: Duration,
    min_colours: usize,
    windows: bool,
    evidence: &Path,
    name: &str,
) -> Option<(Reached, Duration, usize, usize, u64, usize)> {
    let t0 = Instant::now();
    let mut next_png = Duration::ZERO;
    let mut text_streak = 0;
    let mut prev: Option<Vec<u8>> = None;
    while t0.elapsed() < limit {
        tokio::time::sleep(Duration::from_secs(1)).await;
        let (w, h, rgb) = snap(s);
        let (lit, n, top_lit) = lit_pixels(&rgb, w);
        let colours = distinct_colours(&rgb);
        if t0.elapsed() >= next_png {
            save_png(
                s,
                &evidence.join(format!("{name}-{:03}s.png", t0.elapsed().as_secs())),
            );
            next_png += Duration::from_secs(30);
        }
        let cls = classify_frame(lit, n, top_lit, colours, min_colours);
        match cls {
            FrameClass::Graphical => {
                return Some((Reached::Graphical, t0.elapsed(), w, h, lit, colours));
            }
            FrameClass::Stop => return Some((Reached::Stop, t0.elapsed(), w, h, lit, colours)),
            _ => {}
        }
        let text_like = matches!(cls, FrameClass::TextLike);
        let still = prev
            .as_ref()
            .is_some_and(|p| changed_bytes(p, &rgb) <= (rgb.len() / 1000 + 256) as u64);
        text_streak = if text_like && still {
            text_streak + 1
        } else {
            0
        };
        prev = Some(rgb);
        if !windows && text_streak >= 15 {
            return Some((Reached::Text, t0.elapsed(), w, h, lit, colours));
        }
        if let Some(e) = &s.lock().unwrap().ended {
            eprintln!("console ended: {e}");
            return None;
        }
    }
    None
}

/// QEMU processes whose command line mentions `marker` (the private root of
/// this test: every run and probe of the test has its sockets there).
fn qemu_processes_with(marker: &str) -> Vec<String> {
    if !cfg!(windows) {
        return Vec::new();
    }
    let script = "Get-CimInstance Win32_Process -Filter \"Name='qemu-system-x86_64.exe'\" | \
                  ForEach-Object { \"$($_.ProcessId) $($_.CommandLine)\" }";
    let out = std::process::Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", script])
        .output()
        .expect("process list");
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|l| l.contains(marker))
        .map(|l| l.chars().take(200).collect())
        .collect()
}

// ----- VMware import -------------------------------------------------------

async fn preview(
    c: &mut Client,
    vmx: &str,
) -> Result<lightvm_proto::ipc::VmwareImportPreview, lightvm_proto::PublicError> {
    let m = req(
        c,
        Request::PreviewVmwareImport {
            vmx_path: vmx.into(),
        },
    )
    .await;
    match m {
        ServerMessage::Response {
            result: ResponseBody::VmwareImportPreview { preview },
            ..
        } => Ok(*preview),
        other => Err(error(&other).unwrap_or_else(|| panic!("{other:?}"))),
    }
}

fn error(m: &ServerMessage) -> Option<lightvm_proto::PublicError> {
    match m {
        ServerMessage::Error {
            error: ReplyError::Operational(p),
            ..
        } => Some(p.clone()),
        _ => None,
    }
}

/// SHA-256 of every file of the VMware folder (name, bytes), streamed in
/// chunks (the disks are multi-gigabyte files).
fn file_hash(p: &Path) -> String {
    use sha2::Digest as _;
    let mut h = sha2::Sha256::new();
    let mut f = std::fs::File::open(p).unwrap();
    let mut buf = vec![0u8; 4 << 20];
    loop {
        let n = std::io::Read::read(&mut f, &mut buf).unwrap();
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    hex_encode(&h.finalize())
}

fn folder_hashes(folder: &Path) -> Vec<(String, String)> {
    let mut v = std::fs::read_dir(folder)
        .unwrap()
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
        .map(|e| {
            let n = e.file_name().to_string_lossy().into_owned();
            (n, file_hash(&e.path()))
        })
        .collect::<Vec<_>>();
    v.sort();
    v
}

fn hex_encode(b: &[u8]) -> String {
    let mut s = String::with_capacity(b.len() * 2);
    for x in b {
        s.push_str(&format!("{x:02x}"));
    }
    s
}

/// `qemu-img compare` of the converted catalog disk with the source chain.
fn content_identical(q: &Path, top: &Path, disk: &Path) -> bool {
    let o = std::process::Command::new(q.join(exe("qemu-img")))
        .args(["compare", "-q", "-f", "vmdk", "-F", "qcow2"])
        .arg(top)
        .arg(disk)
        .output()
        .expect("qemu-img compare");
    o.status.success()
}

/// Nothing of an import left in the catalog: `tmp/` empty.
fn nothing_left(dir: &Path) {
    let tmp: Vec<_> = std::fs::read_dir(dir.join("tmp"))
        .map(|r| r.flatten().map(|e| e.file_name()).collect())
        .unwrap_or_default();
    assert!(tmp.is_empty(), "tmp/ not empty: {tmp:?}");
}

/// Per-VM record written to the evidence directory.
#[derive(Default)]
struct Record {
    name: String,
    preview: String,
    imported: Option<String>,
    source_unchanged: Option<bool>,
    content_identical: Option<bool>,
    running_after: Option<String>,
    desktop_after: Option<String>,
    console_after: Option<String>,
    stop_after: Option<String>,
    final_frame: Option<String>,
    stopped: bool,
    orphans: Vec<String>,
    note: String,
}

impl Record {
    fn line(&self) -> String {
        let mut l = format!(
            "{} | preview={} | imported={} | source_unchanged={:?} | content_identical={:?} \
             | running_after={:?} | desktop_after={:?} | console_after={:?} | stop_after={:?} \
             | final_frame={:?} | stopped={}",
            self.name,
            self.preview,
            self.imported.as_deref().unwrap_or("-"),
            self.source_unchanged,
            self.content_identical,
            self.running_after,
            self.desktop_after,
            self.console_after,
            self.stop_after,
            self.final_frame,
            self.stopped
        );
        if !self.orphans.is_empty() {
            l.push_str(&format!(" | orphans={:?}", self.orphans));
        }
        if !self.note.is_empty() {
            l.push_str(&format!(" | note={}", self.note));
        }
        l
    }
}

#[test]
#[ignore = "requires approved QEMU, the accelerator and real VMware VMs (LIGHTVM_E2E_QEMU_DIR, LIGHTVM_E2E_VMX)"]
fn e2e_real_vmware_vm_import_and_boot() {
    let q = qemu_dir();
    let vmxs: Vec<PathBuf> = std::env::split_paths(
        &std::env::var_os("LIGHTVM_E2E_VMX").expect("LIGHTVM_E2E_VMX not set"),
    )
    .collect();
    assert!(!vmxs.is_empty(), "no .vmx in LIGHTVM_E2E_VMX");
    let desktop_min = std::env::var("LIGHTVM_VM_DESKTOP_MIN")
        .ok()
        .and_then(|m| m.parse().ok())
        .unwrap_or(10);
    let expect_desktop: Vec<String> = std::env::var_os("LIGHTVM_VM_EXPECT_DESKTOP")
        .map(|v| {
            std::env::split_paths(&v)
                .map(|p| {
                    p.file_stem()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .into_owned()
                })
                .collect()
        })
        .unwrap_or_default();
    let expect_refused: Vec<String> = std::env::var_os("LIGHTVM_VM_EXPECT_REFUSED")
        .map(|v| {
            std::env::split_paths(&v)
                .map(|p| {
                    p.file_stem()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .into_owned()
                })
                .collect()
        })
        .unwrap_or_default();
    let evidence = std::env::var_os("LIGHTVM_VM_EVIDENCE_DIR").map_or_else(
        || Path::new(env!("CARGO_MANIFEST_DIR")).join("../../lab/logs/vmware-real"),
        PathBuf::from,
    );
    std::fs::create_dir_all(&evidence).unwrap();
    let dir = temp_dir("vmw-real");
    let _cleanup = RemoveCatalog(dir.clone());
    let private_root = short_dir("lvw");
    let marker = private_root
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let mut records: Vec<Record> = Vec::new();

    let store = SqliteStore::open(&dir).unwrap();
    let mut setup = QemuSetup::new(qemu_dir(), store.root(), private_root.clone());
    setup.cpu_model =
        std::env::var("LIGHTVM_E2E_CPU").unwrap_or_else(|_| "Skylake-Client-v4".into());
    // A live guest may ignore the ACPI power button: a short escalation.
    setup.stop_policy = qemu_backend::ShutdownPolicy {
        powerdown_timeout: Duration::from_secs(20),
        quit_timeout: Duration::from_secs(5),
        kill_timeout: Duration::from_secs(10),
    };
    if let Some(h) = std::env::var_os("LIGHTVM_E2E_HELPER_DIR") {
        setup.storage_helper_dir = Some(PathBuf::from(h));
    }
    let (host, vm_backend, capacity) = probe(setup).expect("QEMU probe");
    assert!(
        capacity.accelerator_available,
        "reference accelerator probe"
    );
    let audit = FileAuditSink::open(&dir.join("audit").join("audit.jsonl")).unwrap();
    let inspector = ManagedMediaInspector::new(store.root());
    let engine =
        CoreEngine::new(store, audit, vm_backend, inspector, core_config(capacity)).unwrap();
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let mut failures = Vec::new();
    rt.block_on(async {
        let mut cfg = ServiceConfig::new(&dir);
        cfg.runtime_dir = Some(dir.join("run"));
        cfg.display_dir = Some(short_dir("lvd"));
        let (handle, ui) = start(cfg, Box::new(engine), Box::new(host)).await.unwrap();
        let mut c = tokio::time::timeout(T, Client::connect(handle.endpoint()))
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(
            c.hello(&ui).await.unwrap(),
            ServerMessage::HelloOk { role: Role::Ui, .. }
        ));

        for vmx in &vmxs {
            let vmx = resolved(vmx);
            let name = vmx
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default();
            let vmx_str = vmx.to_string_lossy().into_owned();
            let folder = vmx.parent().unwrap().to_path_buf();
            let mut rec = Record {
                name: name.clone(),
                ..Record::default()
            };
            let before = folder_hashes(&folder);
            let is_windows;

            // 1. Preview.
            match preview(&mut c, &vmx_str).await {
                Ok(p) => {
                    let prep = p
                        .config
                        .imported
                        .as_ref()
                        .is_some_and(|h| h.storage_driver_preparation);
                    is_windows = matches!(
                        p.config.guest_os,
                        GuestOs::WindowsXp
                            | GuestOs::WindowsVista
                            | GuestOs::Windows7
                            | GuestOs::Windows81
                            | GuestOs::Windows10
                            | GuestOs::Windows11Arm64
                    );
                    rec.preview = format!(
                        "ok: guest={:?} firmware={:?} controller={:?} levels={} \
                         virtual={} bytes prep={} warnings={:?} required={:?} free={:?}",
                        p.config.guest_os,
                        p.config.firmware,
                        p.disk.vmware_controller,
                        p.disk.snapshot_levels,
                        p.disk.virtual_size_bytes,
                        prep,
                        p.warnings.iter().map(|w| w.kind).collect::<Vec<_>>(),
                        p.required_bytes,
                        p.free_bytes
                    );
                    if prep {
                        rec.note = "storage driver preparation needed for boot (ADR-0030)".into();
                    }
                }
                Err(e) => {
                    rec.preview = format!("refused: {} / {}", e.code, e.message);
                    if expect_refused.contains(&name) {
                        eprintln!("{name}: preview refused as expected: {e:?}");
                        records.push(rec);
                        continue;
                    }
                    failures.push(format!("{name}: preview refused: {e:?}"));
                    records.push(rec);
                    continue;
                }
            }

            // 2. Import.
            let imp_name = format!("{name} imported");
            let r = req(
                &mut c,
                Request::ImportVmware {
                    vmx_path: vmx_str,
                    name: VmName::parse(&imp_name).unwrap(),
                    display: DisplayAdapter::Standard,
                },
            )
            .await;
            let vm = match (expect_refused.contains(&name), r) {
                (true, _) => unreachable!("refusal handled above"),
                (
                    _,
                    ServerMessage::Response {
                        result: ResponseBody::VmwareImported { vm },
                        ..
                    },
                ) => vm,
                (_, other) => {
                    failures.push(format!("{name}: import refused: {other:?}"));
                    records.push(rec);
                    continue;
                }
            };
            rec.imported = Some(imp_name);
            // The row carries the preparation outcome: the flag is cleared
            // when the storage helper enabled the driver in the disk.
            let prepared = vm
                .config
                .imported
                .as_ref()
                .is_some_and(|h| !h.storage_driver_preparation);
            if prepared {
                rec.note = format!("{}; storage driver enabled in the converted disk", rec.note);
            }

            // 3. Source unchanged, converted content identical (the prepared
            // disk differs from the source by design: the SYSTEM hive edit).
            let after = folder_hashes(&folder);
            rec.source_unchanged = Some(before == after);
            let top = vmware_import::inspect(&resolved(&vmx).to_string_lossy())
                .map(|i| i.disk.top)
                .unwrap_or_else(|e| {
                    failures.push(format!("{name}: source re-inspection failed: {e:?}"));
                    PathBuf::from("/")
                });
            let disk = dir
                .join("vms")
                .join(vm.id.to_string())
                .join("primary.qcow2");
            if !prepared {
                let same = disk.is_file() && content_identical(&q, &top, &disk);
                rec.content_identical = Some(same);
                if !same {
                    failures.push(format!("{name}: converted disk differs from the source"));
                }
            } else if !disk.is_file() {
                failures.push(format!("{name}: converted disk missing"));
            }

            // 4. Start and watch.
            let t = Instant::now();
            let m = submit(&mut c, vm.id, VmAction::Start).await;
            if !op_ok(&m) {
                failures.push(format!("{name}: start refused: {m:?}"));
                records.push(rec);
                continue;
            }
            if !wait_state(&mut c, vm.id, VmState::Running, T).await {
                failures.push(format!("{name}: not running"));
                records.push(rec);
                continue;
            }
            rec.running_after = Some(format!("{:?}", t.elapsed()));
            eprintln!("{name}: running after {:?}", t.elapsed());
            let ticket = match req(&mut c, Request::OpenConsole { vm_id: vm.id }).await {
                ServerMessage::Response {
                    result: ResponseBody::Console { ticket },
                    ..
                } => ticket,
                other => panic!("{name}: {other:?}"),
            };
            let screen = watch(&ticket);
            let reached = wait_reached(
                &screen,
                Duration::from_secs(60 * desktop_min),
                16,
                is_windows,
                &evidence,
                &name,
            )
            .await;
            match reached {
                Some((Reached::Graphical, after, w, h, lit, colours)) => {
                    tokio::time::sleep(Duration::from_secs(10)).await;
                    let png = evidence.join(format!("{name}-desktop.png"));
                    save_png(&screen, &png);
                    rec.desktop_after = Some(format!("{after:?}"));
                    eprintln!(
                        "{name}: desktop-like frame {w}x{h} with {lit} lit pixels and {colours} \
                         colours after {after:?}; saved {}",
                        png.display()
                    );
                }
                Some((Reached::Text, after, w, h, lit, colours)) => {
                    let png = evidence.join(format!("{name}-login.png"));
                    save_png(&screen, &png);
                    rec.console_after = Some(format!("{after:?}"));
                    eprintln!(
                        "{name}: text login prompt {w}x{h} with {lit} lit pixels and {colours} \
                         colours after {after:?}; saved {}",
                        png.display()
                    );
                }
                Some((Reached::Stop, after, w, h, lit, colours)) => {
                    let png = evidence.join(format!("{name}-stop.png"));
                    save_png(&screen, &png);
                    rec.stop_after = Some(format!("{after:?}"));
                    eprintln!(
                        "{name}: stop-like screen {w}x{h} with {lit} lit pixels and {colours} \
                         colours after {after:?}; saved {}",
                        png.display()
                    );
                }
                None => {
                    let (w, h, rgb) = snap(&screen);
                    let (lit, n, _) = lit_pixels(&rgb, w);
                    rec.final_frame = Some(format!(
                        "{w}x{h} lit={lit}/{n} colours={}",
                        distinct_colours(&rgb)
                    ));
                    save_png(&screen, &evidence.join(format!("{name}-last.png")));
                    eprintln!(
                        "{name}: no session entry within {} min; last frame {}",
                        desktop_min,
                        rec.final_frame.clone().unwrap_or_default()
                    );
                }
            }
            if expect_desktop.contains(&name) {
                assert!(
                    rec.desktop_after.is_some() || rec.console_after.is_some(),
                    "{name}: a session entry was expected"
                );
            }

            // 5. Stop and clean state.
            let m = submit(&mut c, vm.id, VmAction::Stop).await;
            let stopped = op_ok(&m)
                && wait_state(&mut c, vm.id, VmState::Stopped, Duration::from_secs(60)).await;
            if stopped {
                eprintln!("{name}: stopped");
            } else {
                eprintln!("{name}: graceful stop not completed ({m:?}): forced stop");
                let m = submit(&mut c, vm.id, VmAction::ForceStop).await;
                assert!(op_ok(&m), "{name}: force stop: {m:?}");
                assert!(
                    wait_state(&mut c, vm.id, VmState::Stopped, T).await,
                    "{name}: not stopped"
                );
            }
            rec.stopped = true;
            rec.orphans = qemu_processes_with(&marker);
            if !rec.orphans.is_empty() {
                failures.push(format!("{name}: QEMU processes left: {:?}", rec.orphans));
            }
            nothing_left(&dir);
            records.push(rec);
        }
        handle.shutdown().await;
        let left = qemu_processes_with(&marker);
        assert!(left.is_empty(), "orphan QEMU processes: {left:?}");
    });
    let _ = std::fs::remove_dir_all(&private_root);

    // Audit: one vm_import per imported VM, none left in the queue.
    let audit_events: Vec<AuditEvent> =
        std::fs::read_to_string(dir.join("audit").join("audit.jsonl"))
            .unwrap_or_default()
            .lines()
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect();
    let imported_n = records.iter().filter(|r| r.imported.is_some()).count();
    let ok_n = audit_events
        .iter()
        .filter(|e| e.action == AuditAction::VmImport && e.outcome == AuditOutcome::Succeeded)
        .count();
    assert_eq!(ok_n, imported_n, "audit vm_import: {audit_events:?}");

    // Evidence for the dated verification section.
    let mut text = String::new();
    for r in &records {
        text.push_str(&format!("{}\n", r.line()));
    }
    let out = evidence.join("vmware-real-results.txt");
    std::fs::write(&out, text).unwrap();
    eprintln!("records: {}", out.display());
    for r in &records {
        eprintln!("{}", r.line());
    }
    assert!(failures.is_empty(), "{failures:#?}");
}

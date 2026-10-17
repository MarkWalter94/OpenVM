//! Windows hardware profiles with real QEMU through the service (ignored
//! test, ADR-0027): a VM declared Windows 7 (legacy BIOS: q35, SeaBIOS,
//! AHCI disk, VGA, UHCI tablet) and one declared Windows 10 (UEFI: q35,
//! OVMF, AHCI disk, VGA, xHCI tablet) boot the reference Ubuntu ISO as
//! installation media (no Windows medium is needed to exercise the profile:
//! the firmware, the controllers and the display are those of the Windows
//! profile). Each one starts, reaches `running`, shows a frame that is not
//! black on the console, stops (gracefully or, failing that, forcibly), and
//! leaves no QEMU process behind.
//!
//! Requires `LIGHTVM_E2E_QEMU_DIR`; optional `LIGHTVM_E2E_ISO` (default
//! `lab/media/ubuntu-26.04.1-desktop-amd64.iso`), `LIGHTVM_E2E_CPU`
//! (`Skylake-Client-v4`), `LIGHTVM_WIN_EVIDENCE_DIR` (PNG of the frames).

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use lightvm_proto::ipc::{ConsoleTicket, Request, ResponseBody, Role, ServerMessage, VmSummary};
use lightvm_proto::{
    Firmware, GuestOs, MediaKind, OperationId, OperationStatus, VmAction, VmId, VmRequest, VmState,
};
use lightvm_service::adapters::audit::FileAuditSink;
use lightvm_service::adapters::backend::{QemuSetup, probe};
use lightvm_service::adapters::catalog::{ManagedMediaInspector, SqliteStore};
use lightvm_service::client::Client;
use lightvm_service::engine::{CoreEngine, core_config};
use lightvm_service::fakes::sample_guest_config;
use lightvm_service::test_support::temp_dir;
use lightvm_service::{ServiceConfig, start};

const T: Duration = Duration::from_secs(120);

fn qemu_dir() -> PathBuf {
    PathBuf::from(std::env::var_os("LIGHTVM_E2E_QEMU_DIR").expect("LIGHTVM_E2E_QEMU_DIR not set"))
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

async fn import(c: &mut Client, path: &Path, kind: MediaKind) -> lightvm_proto::MediaRecord {
    let p = std::fs::canonicalize(path).unwrap();
    let p = p.to_str().unwrap().trim_start_matches(r"\\?\").to_owned();
    match req(
        c,
        Request::ImportMedia {
            source_path: p,
            media_kind: kind,
            declared_origin: "lab media".into(),
        },
    )
    .await
    {
        ServerMessage::Response {
            result: ResponseBody::MediaImported { media },
            ..
        } => media,
        other => panic!("{other:?}"),
    }
}

// ----- console framebuffer -------------------------------------------------

#[derive(Default)]
struct Screen {
    width: usize,
    height: usize,
    rgb: Vec<u8>,
    ended: Option<String>,
}

fn watch(ticket: &ConsoleTicket) -> Arc<Mutex<Screen>> {
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

/// Lit pixels (any channel above 16) in thousandths of the frame.
fn non_black_permille(rgb: &[u8]) -> u64 {
    let px = rgb.as_chunks::<3>().0;
    let n = px.len().max(1) as u64;
    let lit = px.iter().filter(|p| p.iter().any(|&c| c > 16)).count() as u64;
    lit * 1000 / n
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

/// Waits for a frame with at least 1 % lit pixels; returns the time and the
/// geometry and lit share of that frame.
async fn wait_not_black(
    s: &Arc<Mutex<Screen>>,
    limit: Duration,
) -> Option<(Duration, usize, usize, u64)> {
    let t0 = Instant::now();
    while t0.elapsed() < limit {
        tokio::time::sleep(Duration::from_secs(1)).await;
        let (w, h, rgb) = snap(s);
        let lit = non_black_permille(&rgb);
        if w > 0 && lit >= 10 {
            return Some((t0.elapsed(), w, h, lit));
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

#[test]
#[ignore = "requires approved QEMU, WHPX and the Ubuntu ISO (LIGHTVM_E2E_QEMU_DIR)"]
fn e2e_real_qemu_windows_profiles_boot_and_stop() {
    let lab = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../lab");
    let iso = std::env::var_os("LIGHTVM_E2E_ISO").map_or_else(
        || lab.join("media").join("ubuntu-26.04.1-desktop-amd64.iso"),
        PathBuf::from,
    );
    let cpu = std::env::var("LIGHTVM_E2E_CPU").unwrap_or_else(|_| "Skylake-Client-v4".into());
    let evidence = std::env::var_os("LIGHTVM_WIN_EVIDENCE_DIR").map_or_else(
        || std::env::temp_dir().join("lightvm-win-evidence"),
        PathBuf::from,
    );
    std::fs::create_dir_all(&evidence).unwrap();
    let dir = temp_dir("win-real");
    let _cleanup = RemoveCatalog(dir.clone());
    let private_root = short_dir("lvw");
    let marker = private_root
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned();

    let store = SqliteStore::open(&dir).unwrap();
    let mut setup = QemuSetup::new(qemu_dir(), store.root(), private_root.clone());
    setup.cpu_model = cpu;
    // A live ISO at its boot menu may ignore ACPI: a short escalation.
    setup.stop_policy = qemu_backend::ShutdownPolicy {
        powerdown_timeout: Duration::from_secs(20),
        quit_timeout: Duration::from_secs(5),
        kill_timeout: Duration::from_secs(10),
    };
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
        let t = Instant::now();
        let install = import(&mut c, &iso, MediaKind::InstallIso).await;
        eprintln!(
            "imported in {:?}: detected guest {:?}",
            t.elapsed(),
            install.detected_guest
        );

        let mut failures = Vec::new();
        for (name, os, fw) in [
            ("win7-bios", GuestOs::Windows7, Firmware::Bios),
            ("win10-uefi", GuestOs::Windows10, Firmware::Uefi),
        ] {
            let mut vmc = sample_guest_config(name, os, fw);
            vmc.install_media = Some(install.id);
            let vm = match req(&mut c, Request::CreateVm { config: vmc }).await {
                ServerMessage::Response {
                    result: ResponseBody::VmCreated { vm },
                    ..
                } => vm.id,
                other => panic!("{name}: {other:?}"),
            };
            let t = Instant::now();
            let m = submit(&mut c, vm, VmAction::Start).await;
            if !op_ok(&m) {
                failures.push(format!("{name}: start refused: {m:?}"));
                continue;
            }
            if !wait_state(&mut c, vm, VmState::Running, T).await {
                failures.push(format!("{name}: not running"));
                continue;
            }
            eprintln!("{name}: running after {:?}", t.elapsed());
            let ticket = match req(&mut c, Request::OpenConsole { vm_id: vm }).await {
                ServerMessage::Response {
                    result: ResponseBody::Console { ticket },
                    ..
                } => ticket,
                other => panic!("{name}: {other:?}"),
            };
            let screen = watch(&ticket);
            match wait_not_black(&screen, Duration::from_secs(180)).await {
                Some((after, w, h, lit)) => {
                    // A later frame too (boot menu or kernel splash).
                    tokio::time::sleep(Duration::from_secs(20)).await;
                    let png = evidence.join(format!("{name}.png"));
                    save_png(&screen, &png);
                    eprintln!(
                        "{name}: frame {w}x{h} with {lit}/1000 lit pixels after {after:?}; saved {}",
                        png.display()
                    );
                }
                None => {
                    save_png(&screen, &evidence.join(format!("{name}-black.png")));
                    failures.push(format!("{name}: only black frames"));
                }
            }
            let t = Instant::now();
            let m = submit(&mut c, vm, VmAction::Stop).await;
            let stopped = op_ok(&m)
                && wait_state(&mut c, vm, VmState::Stopped, Duration::from_secs(60)).await;
            if stopped {
                eprintln!("{name}: stopped in {:?}", t.elapsed());
            } else {
                eprintln!("{name}: graceful stop not completed ({m:?}): forced stop");
                let m = submit(&mut c, vm, VmAction::ForceStop).await;
                assert!(op_ok(&m), "{name}: force stop: {m:?}");
                assert!(
                    wait_state(&mut c, vm, VmState::Stopped, T).await,
                    "{name}: not stopped"
                );
                eprintln!("{name}: force-stopped after {:?}", t.elapsed());
            }
            let left = qemu_processes_with(&marker);
            if !left.is_empty() {
                failures.push(format!("{name}: QEMU processes left: {left:?}"));
            }
            // The legacy BIOS VM has no UEFI variables.
            let vars = dir
                .join("vms")
                .join(vm.to_string())
                .join("firmware")
                .join("primary.vars.fd");
            assert_eq!(vars.exists(), fw == Firmware::Uefi, "{name}: UEFI variables");
        }
        handle.shutdown().await;
        let left = qemu_processes_with(&marker);
        assert!(left.is_empty(), "orphan QEMU processes: {left:?}");
        assert!(failures.is_empty(), "{failures:#?}");
    });
    let _ = std::fs::remove_dir_all(&private_root);
}

/// Distinct colours in the frame: a stop error screen or a firmware shell
/// has a handful (text without anti-aliasing), the Windows Setup window
/// hundreds (icons, anti-aliased text, gradients).
fn distinct_colours(rgb: &[u8]) -> usize {
    rgb.as_chunks::<3>()
        .0
        .iter()
        .collect::<std::collections::HashSet<_>>()
        .len()
}

/// Waits for a mostly lit frame with at least 64 distinct colours (the
/// Windows Setup window, unlike the text screens before it and a stop error
/// screen; the Windows 10 background is one flat colour, so the share of a
/// single colour does not tell them apart) and saves a PNG every 30 s on
/// the way.
async fn wait_lit(
    s: &Arc<Mutex<Screen>>,
    permille: u64,
    limit: Duration,
    evidence: &Path,
    name: &str,
) -> Option<(Duration, usize, usize, u64)> {
    let t0 = Instant::now();
    let mut next_png = Duration::ZERO;
    while t0.elapsed() < limit {
        tokio::time::sleep(Duration::from_secs(1)).await;
        let (w, h, rgb) = snap(s);
        let lit = non_black_permille(&rgb);
        if t0.elapsed() >= next_png {
            save_png(
                s,
                &evidence.join(format!("{name}-{:03}s.png", t0.elapsed().as_secs())),
            );
            next_png += Duration::from_secs(30);
        }
        if w > 0 && lit >= permille && distinct_colours(&rgb) >= 64 {
            return Some((t0.elapsed(), w, h, lit));
        }
        if let Some(e) = &s.lock().unwrap().ended {
            eprintln!("console ended: {e}");
            return None;
        }
    }
    None
}

/// Real Windows installation media through the service, as the wizard uses
/// them (ADR-0027): each ISO of `LIGHTVM_E2E_WINDOWS_ISOS` (list separated
/// like `PATH`) is imported and must be detected as a Windows system; a VM
/// with the detected system, its suggested resources and its recommended
/// firmware for the detected architecture is refused if this host cannot run
/// it (Windows Vista 64-bit on Windows hosts); otherwise it starts, reaches
/// `running` (UEFI media on a blank disk: the backend answers the CD boot
/// prompt) and
/// shows Windows Setup (a mostly lit frame) within
/// `LIGHTVM_E2E_SETUP_MIN` minutes (default 10); then it is stopped and no
/// QEMU process is left. Frames are saved in `LIGHTVM_WIN_EVIDENCE_DIR`.
#[test]
#[ignore = "requires approved QEMU, WHPX and Windows ISOs (LIGHTVM_E2E_QEMU_DIR, LIGHTVM_E2E_WINDOWS_ISOS)"]
fn e2e_real_windows_iso_reaches_setup() {
    let isos: Vec<PathBuf> = std::env::split_paths(
        &std::env::var_os("LIGHTVM_E2E_WINDOWS_ISOS").expect("LIGHTVM_E2E_WINDOWS_ISOS not set"),
    )
    .collect();
    assert!(!isos.is_empty(), "no ISO in LIGHTVM_E2E_WINDOWS_ISOS");
    let cpu = std::env::var("LIGHTVM_E2E_CPU").unwrap_or_else(|_| "Skylake-Client-v4".into());
    let setup_limit = Duration::from_secs(
        60 * std::env::var("LIGHTVM_E2E_SETUP_MIN")
            .ok()
            .and_then(|m| m.parse().ok())
            .unwrap_or(10),
    );
    let evidence = std::env::var_os("LIGHTVM_WIN_EVIDENCE_DIR").map_or_else(
        || std::env::temp_dir().join("lightvm-win-evidence"),
        PathBuf::from,
    );
    std::fs::create_dir_all(&evidence).unwrap();
    let dir = temp_dir("win-setup");
    let _cleanup = RemoveCatalog(dir.clone());
    let private_root = short_dir("lvw");
    let marker = private_root
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned();

    let store = SqliteStore::open(&dir).unwrap();
    let mut setup = QemuSetup::new(qemu_dir(), store.root(), private_root.clone());
    setup.cpu_model = cpu;
    // Windows Setup ignores the ACPI power button: a short escalation.
    setup.stop_policy = qemu_backend::ShutdownPolicy {
        powerdown_timeout: Duration::from_secs(20),
        quit_timeout: Duration::from_secs(5),
        kill_timeout: Duration::from_secs(10),
    };
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

        let mut failures = Vec::new();
        for (i, iso) in isos.iter().enumerate() {
            let t = Instant::now();
            let install = import(&mut c, iso, MediaKind::InstallIso).await;
            let Some(guest) = install.detected_guest.clone() else {
                failures.push(format!("{}: not detected", iso.display()));
                continue;
            };
            eprintln!(
                "{}: imported in {:?}, detected {guest:?}",
                iso.display(),
                t.elapsed()
            );
            let os = guest.os;
            if !format!("{os:?}").starts_with("Windows") {
                failures.push(format!("{}: detected {os:?}", iso.display()));
                continue;
            }
            let arch = guest.arch.map_or_else(|| "x".into(), |a| format!("{a:?}"));
            let name = format!("win-{i}-{}", arch.to_lowercase().replace('_', "-"));
            let fw = os.recommended_firmware(guest.arch);
            let mut vmc = sample_guest_config(&name, os, fw);
            vmc.install_media = Some(install.id);
            let created = req(&mut c, Request::CreateVm { config: vmc }).await;
            // Media this host cannot run (Vista x64 under WHPX, ADR-0027):
            // the service refuses the VM.
            if !os.runs_on_this_host_with(guest.arch) {
                match created {
                    ServerMessage::Response {
                        result: ResponseBody::VmCreated { .. },
                        ..
                    } => failures.push(format!("{name}: {os:?} {arch} created on this host")),
                    other => eprintln!("{name}: {os:?} {arch} refused on this host: {other:?}"),
                }
                continue;
            }
            let vm = match created {
                ServerMessage::Response {
                    result: ResponseBody::VmCreated { vm },
                    ..
                } => vm.id,
                other => panic!("{name}: {other:?}"),
            };
            eprintln!("{name}: {os:?} {arch} with {fw:?}");
            let t = Instant::now();
            let m = submit(&mut c, vm, VmAction::Start).await;
            if !op_ok(&m) {
                failures.push(format!("{name}: start refused: {m:?}"));
                continue;
            }
            if !wait_state(&mut c, vm, VmState::Running, T).await {
                failures.push(format!("{name}: not running"));
                continue;
            }
            eprintln!("{name}: running after {:?}", t.elapsed());
            let ticket = match req(&mut c, Request::OpenConsole { vm_id: vm }).await {
                ServerMessage::Response {
                    result: ResponseBody::Console { ticket },
                    ..
                } => ticket,
                other => panic!("{name}: {other:?}"),
            };
            let screen = watch(&ticket);
            match wait_lit(&screen, 500, setup_limit, &evidence, &name).await {
                Some((after, w, h, lit)) => {
                    // The Setup window is drawn shortly after its background.
                    tokio::time::sleep(Duration::from_secs(15)).await;
                    let png = evidence.join(format!("{name}-setup.png"));
                    save_png(&screen, &png);
                    eprintln!(
                        "{name}: frame {w}x{h} with {lit}/1000 lit pixels after {after:?}; saved {}",
                        png.display()
                    );
                }
                None => {
                    save_png(&screen, &evidence.join(format!("{name}-last.png")));
                    failures.push(format!("{name}: Windows Setup not shown"));
                }
            }
            let t = Instant::now();
            let m = submit(&mut c, vm, VmAction::Stop).await;
            let stopped = op_ok(&m)
                && wait_state(&mut c, vm, VmState::Stopped, Duration::from_secs(60)).await;
            if stopped {
                eprintln!("{name}: stopped in {:?}", t.elapsed());
            } else {
                eprintln!("{name}: graceful stop not completed ({m:?}): forced stop");
                let m = submit(&mut c, vm, VmAction::ForceStop).await;
                assert!(op_ok(&m), "{name}: force stop: {m:?}");
                assert!(
                    wait_state(&mut c, vm, VmState::Stopped, T).await,
                    "{name}: not stopped"
                );
                eprintln!("{name}: force-stopped after {:?}", t.elapsed());
            }
            let left = qemu_processes_with(&marker);
            if !left.is_empty() {
                failures.push(format!("{name}: QEMU processes left: {left:?}"));
            }
        }
        handle.shutdown().await;
        let left = qemu_processes_with(&marker);
        assert!(left.is_empty(), "orphan QEMU processes: {left:?}");
        assert!(failures.is_empty(), "{failures:#?}");
    });
    let _ = std::fs::remove_dir_all(&private_root);
}

async fn open_console(c: &mut Client, vm_id: VmId) -> ConsoleTicket {
    match req(c, Request::OpenConsole { vm_id }).await {
        ServerMessage::Response {
            result: ResponseBody::Console { ticket },
            ..
        } => ticket,
        other => panic!("{other:?}"),
    }
}

/// R6-05 §17: Windows 11 x64 through the service. The Reset action is a
/// cold reboot of the same run (new QEMU and swtpm): the VM stays `running`
/// with the same run, the old console ends, and a new ticket for the same
/// run (what the desktop does after a dropped display) shows the guest.
/// Requires `LIGHTVM_E2E_QEMU_DIR`, `LIGHTVM_WIN11_BUNDLE` (development
/// bundle, approved here for this test only) and `LIGHTVM_WIN11_ISO`.
#[test]
#[ignore = "requires approved QEMU, the Windows 11 lab bundle and ISO, WHPX"]
fn e2e_windows11_reset_is_a_cold_reboot_and_the_console_reconnects() {
    let env = |k: &str| PathBuf::from(std::env::var_os(k).unwrap_or_else(|| panic!("{k}")));
    let bundle = env("LIGHTVM_WIN11_BUNDLE");
    let iso = env("LIGHTVM_WIN11_ISO");
    let evidence = std::env::var_os("LIGHTVM_WIN_EVIDENCE_DIR").map_or_else(
        || std::env::temp_dir().join("lightvm-win-evidence"),
        PathBuf::from,
    );
    std::fs::create_dir_all(&evidence).unwrap();
    let dir = temp_dir("win11-e2e");
    let _cleanup = RemoveCatalog(dir.clone());
    let private_root = short_dir("lv11");
    let marker = private_root
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let store = SqliteStore::open(&dir).unwrap();
    let mut setup = QemuSetup::new(qemu_dir(), store.root(), private_root.clone());
    setup.win11_dir = Some(bundle.clone());
    setup
        .probe
        .approved_win11_bundle_sha256
        .push(qemu_backend::probe::sha256_file(&bundle.join("SHA256SUMS")).unwrap());
    setup.stop_policy = qemu_backend::ShutdownPolicy {
        powerdown_timeout: Duration::from_secs(20),
        quit_timeout: Duration::from_secs(5),
        kill_timeout: Duration::from_secs(10),
    };
    let (host, vm_backend, mut capacity) = probe(setup).expect("QEMU probe");
    // Lab host only: the 64 GiB minimum disk of the profile is a sparse
    // qcow2 that Setup barely fills here; the creation check would demand
    // that much free space on the lab volume, so the test raises it.
    capacity.free_storage_gib = capacity
        .free_storage_gib
        .max(capacity.reserved_storage_gib + 64);
    let audit = FileAuditSink::open(&dir.join("audit").join("audit.jsonl")).unwrap();
    let inspector = ManagedMediaInspector::new(store.root());
    let engine =
        CoreEngine::new(store, audit, vm_backend, inspector, core_config(capacity)).unwrap();
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
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
        let install = import(&mut c, &iso, MediaKind::InstallIso).await;
        assert_eq!(
            install.detected_guest.as_ref().map(|d| d.os),
            Some(GuestOs::Windows11X64)
        );
        let mut vmc = sample_guest_config("win11-e2e", GuestOs::Windows11X64, Firmware::Uefi);
        vmc.install_media = Some(install.id);
        vmc.vcpus = 2;
        vmc.memory_mib = 4096;
        vmc.disk_gib = 64;
        let vm = match req(&mut c, Request::CreateVm { config: vmc }).await {
            ServerMessage::Response {
                result: ResponseBody::VmCreated { vm },
                ..
            } => vm.id,
            other => panic!("{other:?}"),
        };
        let m = submit(&mut c, vm, VmAction::Start).await;
        assert!(op_ok(&m), "start: {m:?}");
        assert!(wait_state(&mut c, vm, VmState::Running, T).await);
        let run = status(&mut c, vm).await.run_id.expect("run");
        let screen = watch(&open_console(&mut c, vm).await);
        let setup_shown = wait_lit(
            &screen,
            300,
            Duration::from_secs(300),
            &evidence,
            "win11-before-reset",
        )
        .await;
        assert!(setup_shown.is_some(), "Windows Setup not shown");
        save_png(&screen, &evidence.join("win11-setup.png"));

        let m = req(
            &mut c,
            Request::SubmitVm {
                request: VmRequest {
                    operation_id: OperationId::new_v7(),
                    vm_id: vm,
                    expected_run_id: Some(run),
                    action: VmAction::Reset,
                },
            },
        )
        .await;
        assert!(op_ok(&m), "reset: {m:?}");
        // The old display ends with its QEMU process.
        let t0 = Instant::now();
        while screen.lock().unwrap().ended.is_none() {
            assert!(
                t0.elapsed() < Duration::from_secs(30),
                "old console still open"
            );
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
        // Same run, still running, after the cold reboot.
        tokio::time::sleep(Duration::from_secs(5)).await;
        let st = status(&mut c, vm).await;
        assert_eq!(st.state, VmState::Running, "{st:?}");
        assert_eq!(st.run_id, Some(run), "the reset must keep the run");
        let again = watch(&open_console(&mut c, vm).await);
        // After the reset the firmware shows text on black (CD prompt, no
        // bootable disk yet): any lit pixel on a frame of the new process.
        let t0 = Instant::now();
        let shown = loop {
            tokio::time::sleep(Duration::from_secs(1)).await;
            let (w, h, rgb) = snap(&again);
            if w > 0 && rgb.iter().any(|&c| c > 64) {
                break Some((t0.elapsed(), w, h));
            }
            if again.lock().unwrap().ended.is_some() || t0.elapsed() > Duration::from_secs(120) {
                break None;
            }
        };
        save_png(&again, &evidence.join("win11-after-reset.png"));
        assert!(
            shown.is_some(),
            "no frame on the new console after the reset"
        );
        eprintln!("console after the cold reboot: {shown:?}");

        let m = submit(&mut c, vm, VmAction::ForceStop).await;
        assert!(op_ok(&m), "force stop: {m:?}");
        assert!(wait_state(&mut c, vm, VmState::Stopped, T).await);
        handle.shutdown().await;
        let left = qemu_processes_with(&marker);
        assert!(left.is_empty(), "orphan QEMU processes: {left:?}");
    });
    let _ = std::fs::remove_dir_all(&private_root);
}

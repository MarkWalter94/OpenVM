//! Windows 11 ARM64 on Apple Silicon with real QEMU through the service
//! (ignored test, ADR-0028): a VM declared Windows 11 ARM64 (`virt` +
//! GICv3, `-cpu host` under HVF, the project's Secure Boot edk2, swtpm TPM
//! 2.0, NVMe, CDs on `usb-bot`, `ramfb`, xHCI keyboard and tablet) is
//! created and started through the service; the start succeeds only after
//! the backend verified HVF and the absence of any NIC (`info network`,
//! `query-pci`). Keys are sent through the service to pass the "Press any
//! key to boot from CD" prompt, a `screendump` is taken through the
//! screenshot path and the console frame must not be black (Windows setup,
//! saved as PNG). Then the Emergency Stop: no `qemu-system-aarch64` or
//! `swtpm` of the test may be left. The VM disk is a sparse qcow2 and the
//! whole catalog is deleted at the end.
//!
//! Environment:
//! - `LIGHTVM_E2E_QEMU_DIR` (required): QEMU distribution folder, for
//!   example `/opt/homebrew` (Homebrew prefix: `bin/` + `share/qemu/`);
//! - `LIGHTVM_E2E_FIRMWARE_DIR` (required): folder with
//!   `lightvm-aarch64-{code,vars-blank,vars-win}.fd`;
//! - `LIGHTVM_E2E_APPROVED_HASHES` (optional): development approvals file
//!   (`<sha256> <kind>` lines); default: the `SHA256SUMS` of the firmware
//!   folder, approved explicitly as `firmware` entries (development only);
//! - `LIGHTVM_E2E_WIN11_ARM64_ISO` (optional): Windows 11 ARM64 ISO; without
//!   it the VM starts without media and the frame is the firmware's;
//! - `LIGHTVM_MAC_EVIDENCE_DIR` (optional): where the PNGs are saved.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use lightvm_proto::ipc::{ConsoleTicket, Request, ResponseBody, Role, ServerMessage, VmSummary};
use lightvm_proto::{
    Firmware, GuestOs, InputBatch, InputEvent, KeyAction, MediaKind, OperationId, OperationStatus,
    QCode, RunId, VmAction, VmId, VmRequest, VmState,
};
use lightvm_service::adapters::audit::FileAuditSink;
use lightvm_service::adapters::backend::{QemuSetup, load_approvals, probe};
use lightvm_service::adapters::catalog::{ManagedMediaInspector, SqliteStore};
use lightvm_service::client::Client;
use lightvm_service::engine::{CoreEngine, core_config};
use lightvm_service::fakes::sample_guest_config;
use lightvm_service::test_support::temp_dir;
use lightvm_service::{ServiceConfig, start};

const T: Duration = Duration::from_secs(120);

fn env_path(name: &str) -> Option<PathBuf> {
    std::env::var_os(name).map(PathBuf::from)
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
    tokio::time::timeout(Duration::from_secs(900), c.request(r))
        .await
        .expect("timeout")
        .unwrap()
}

async fn submit(
    c: &mut Client,
    vm_id: VmId,
    run: Option<RunId>,
    action: VmAction,
) -> ServerMessage {
    req(
        c,
        Request::SubmitVm {
            request: VmRequest {
                operation_id: OperationId::new_v7(),
                vm_id,
                expected_run_id: run,
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
    match req(
        c,
        Request::ImportMedia {
            source_path: p.to_str().unwrap().to_owned(),
            media_kind: kind,
            declared_origin: "user's Windows 11 ARM64 medium".into(),
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

fn key(q: &str, down: bool) -> InputEvent {
    InputEvent::Key(KeyAction {
        qcode: QCode::parse(q).unwrap(),
        down,
    })
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

/// Processes whose command line mentions `marker` (the private root of
/// this test: QEMU's sockets and swtpm's control socket are there).
fn processes_with(marker: &str) -> Vec<String> {
    let out = std::process::Command::new("/bin/ps")
        .args(["-axww", "-o", "pid=,command="])
        .output()
        .expect("process list");
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|l| l.contains(marker) && (l.contains("qemu-system") || l.contains("swtpm")))
        .map(|l| l.chars().take(200).collect())
        .collect()
}

/// Development approvals for the test: the explicit file, or the
/// firmware folder's `SHA256SUMS` turned into `firmware` entries.
fn approvals_text(firmware_dir: &Path) -> String {
    if let Some(p) = env_path("LIGHTVM_E2E_APPROVED_HASHES") {
        return std::fs::read_to_string(p).expect("approved hashes file");
    }
    let sums = std::fs::read_to_string(firmware_dir.join("SHA256SUMS"))
        .expect("SHA256SUMS in the firmware folder (or set LIGHTVM_E2E_APPROVED_HASHES)");
    sums.lines()
        .filter_map(|l| l.split_whitespace().next())
        .map(|h| format!("{h} firmware test approval\n"))
        .collect()
}

#[test]
#[ignore = "requires Apple Silicon, QEMU with HVF, swtpm and the AArch64 firmware (LIGHTVM_E2E_QEMU_DIR, LIGHTVM_E2E_FIRMWARE_DIR)"]
fn e2e_real_qemu_windows11_arm64_starts_to_setup() {
    let qemu_dir = env_path("LIGHTVM_E2E_QEMU_DIR").expect("LIGHTVM_E2E_QEMU_DIR not set");
    let firmware_dir =
        env_path("LIGHTVM_E2E_FIRMWARE_DIR").expect("LIGHTVM_E2E_FIRMWARE_DIR not set");
    let iso = env_path("LIGHTVM_E2E_WIN11_ARM64_ISO");
    let evidence = env_path("LIGHTVM_MAC_EVIDENCE_DIR")
        .unwrap_or_else(|| std::env::temp_dir().join("lightvm-mac-evidence"));
    std::fs::create_dir_all(&evidence).unwrap();
    let dir = temp_dir("mac-real");
    let _cleanup = RemoveCatalog(dir.clone());
    let private_root = short_dir("lvm");
    let marker = private_root
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned();

    let store = SqliteStore::open(&dir).unwrap();
    let mut setup = QemuSetup::new(qemu_dir, store.root(), private_root.clone());
    setup.firmware_dir = Some(firmware_dir.clone());
    let n = load_approvals(&mut setup.probe, &approvals_text(&firmware_dir)).unwrap();
    eprintln!("{n} development approvals");
    let (host, vm_backend, mut capacity) = probe(setup).expect("QEMU probe");
    assert!(
        capacity.accelerator_available,
        "HVF probe with the virt + AArch64 firmware reference"
    );
    // The disk is a sparse qcow2: the 64 GiB minimum of Windows 11 is
    // virtual, the host needs only what the guest writes.
    capacity.free_storage_gib = capacity.free_storage_gib.max(1024);
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
        let mut vmc = sample_guest_config("win11-arm64", GuestOs::Windows11Arm64, Firmware::Uefi);
        if let Some(iso) = &iso {
            let t = Instant::now();
            let install = import(&mut c, iso, MediaKind::InstallIso).await;
            eprintln!(
                "imported in {:?}: detected guest {:?}",
                t.elapsed(),
                install.detected_guest
            );
            vmc.install_media = Some(install.id);
        }
        let vm = match req(&mut c, Request::CreateVm { config: vmc }).await {
            ServerMessage::Response {
                result: ResponseBody::VmCreated { vm },
                ..
            } => vm.id,
            other => panic!("create: {other:?}"),
        };
        let t = Instant::now();
        let m = submit(&mut c, vm, None, VmAction::Start).await;
        assert!(op_ok(&m), "start refused: {m:?}");
        assert!(
            wait_state(&mut c, vm, VmState::Running, T).await,
            "not running"
        );
        let run = status(&mut c, vm).await.run_id.expect("run");
        eprintln!(
            "running after {:?} (HVF verified, no NIC in info network / query-pci)",
            t.elapsed()
        );
        let ticket = match req(&mut c, Request::OpenConsole { vm_id: vm }).await {
            ServerMessage::Response {
                result: ResponseBody::Console { ticket },
                ..
            } => ticket,
            other => panic!("console: {other:?}"),
        };
        let screen = watch(&ticket);
        // "Press any key to boot from CD or DVD": a key every second for
        // the first 30 s (keyboard batches do not depend on the geometry).
        let geometry = lightvm_proto::DisplayGeometry::new(800, 600).unwrap();
        for _ in 0..30 {
            let _ = submit(
                &mut c,
                vm,
                Some(run),
                VmAction::Input(InputBatch {
                    vm_id: vm,
                    run_id: run,
                    geometry,
                    events: vec![key("spc", true), key("spc", false)],
                }),
            )
            .await;
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
        save_png(&screen, &evidence.join("win11-arm64-boot.png"));
        // Windows setup: wait for a lit frame that stays lit.
        let t0 = Instant::now();
        let mut lit_since: Option<Instant> = None;
        let mut last = 0;
        while t0.elapsed() < Duration::from_secs(420) {
            tokio::time::sleep(Duration::from_secs(2)).await;
            let (w, _, rgb) = snap(&screen);
            last = non_black_permille(&rgb);
            if w > 0 && last >= 50 {
                let since = *lit_since.get_or_insert_with(Instant::now);
                if since.elapsed() >= Duration::from_secs(60) {
                    break;
                }
            } else {
                lit_since = None;
            }
            if let Some(e) = &screen.lock().unwrap().ended {
                panic!("console ended: {e}");
            }
        }
        let (w, h, _) = snap(&screen);
        let png = evidence.join("win11-arm64-setup.png");
        save_png(&screen, &png);
        eprintln!(
            "frame {w}x{h} with {last}/1000 lit pixels after {:?}; saved {}",
            t.elapsed(),
            png.display()
        );
        assert!(last >= 50, "black frame: see {}", png.display());
        // `screendump` through the product's screenshot path.
        let m = submit(&mut c, vm, Some(run), VmAction::Screenshot).await;
        assert!(op_ok(&m), "screenshot: {m:?}");
        assert!(
            !processes_with(&marker).is_empty(),
            "QEMU and swtpm of the test visible"
        );
        // Emergency Stop: every process of the run ends, none is left.
        match req(&mut c, Request::EmergencyStop).await {
            ServerMessage::Response {
                result: ResponseBody::EmergencyStop(r),
                ..
            } => {
                eprintln!("emergency stop: {r:?}");
                assert!(r.completed);
            }
            other => panic!("{other:?}"),
        }
        assert!(
            wait_state(&mut c, vm, VmState::Stopped, T).await,
            "not stopped after the Emergency Stop"
        );
        let t0 = Instant::now();
        let mut left = processes_with(&marker);
        while !left.is_empty() && t0.elapsed() < Duration::from_secs(10) {
            tokio::time::sleep(Duration::from_millis(200)).await;
            left = processes_with(&marker);
        }
        assert!(left.is_empty(), "orphan processes: {left:?}");
        let tpm = dir.join("vms").join(vm.to_string()).join("tpm");
        assert!(tpm.is_dir(), "TPM state directory");
        let disk = dir.join("vms").join(vm.to_string()).join("primary.qcow2");
        eprintln!(
            "disk {} bytes on the host (sparse qcow2 of 64 GiB)",
            std::fs::metadata(&disk).map_or(0, |m| m.len())
        );
        handle.shutdown().await;
        assert!(processes_with(&marker).is_empty());
    });
    let _ = std::fs::remove_dir_all(&private_root);
}

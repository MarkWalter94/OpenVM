//! F3 exit criterion with real QEMU through the service (ignored tests):
//! baseline with the VM off, N cycles of guest modification and restore
//! with the deterministic fixture of ADR-0019, base hash unchanged.
//!
//! Path chosen (see `docs/verification/F3-01-baseline-restore.md`): the
//! live session of the reference ISO with an empty data disk as baseline
//! (ADR-0019 "Environment and initial state"), the fixture ISO imported as a
//! data medium. Every check is pixel-exact on the RFB framebuffer of the
//! console ticket (lossless 32-bit pixels), with `lightvm_fixture::verify`.
//!
//! Requires `LIGHTVM_E2E_QEMU_DIR`; optional `LIGHTVM_E2E_ISO`,
//! `LIGHTVM_F3_CYCLES` (30), `LIGHTVM_F3_EVIDENCE_DIR` (PNG of the first
//! cycles and of every failure), `LIGHTVM_E2E_CPU` (`Skylake-Client-v4`).

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use lightvm_fixture::verify::{Check, DEFAULT_CALIBRATION, Frame, Marker};
use lightvm_proto::ipc::{
    BaselineSummary, ConsoleTicket, Request, ResponseBody, Role, ServerMessage, VmSummary,
};
use lightvm_proto::{
    AuditAction, AuditEvent, AuditOutcome, InputBatch, InputEvent, KeyAction, Label, MediaKind,
    OperationId, OperationStatus, QCode, RunId, VmAction, VmId, VmRequest, VmState,
};
use lightvm_service::adapters::audit::FileAuditSink;
use lightvm_service::adapters::backend::{QemuSetup, probe};
use lightvm_service::adapters::catalog::{ManagedMediaInspector, SqliteStore};
use lightvm_service::client::Client;
use lightvm_service::engine::{CoreEngine, core_config};
use lightvm_service::fakes::sample_config;
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
        // The supervisor thread may still be releasing the catalog (open
        // database, lock file): retry for a while.
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
    tokio::time::timeout(Duration::from_secs(1800), c.request(r))
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
        } => *vm,
        other => panic!("{other:?}"),
    }
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

// ----- framebuffer --------------------------------------------------------

#[derive(Default)]
struct Screen {
    width: usize,
    height: usize,
    rgb: Vec<u8>,
    seq: u64,
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
                    s.seq += 1;
                }
                Err(e) => {
                    s.ended = Some(e.to_string());
                    return;
                }
            }
            drop(s);
            std::thread::sleep(Duration::from_millis(200));
        }
    });
    seen
}

fn snap(s: &Arc<Mutex<Screen>>) -> (usize, usize, Vec<u8>) {
    let s = s.lock().unwrap();
    (s.width, s.height, s.rgb.clone())
}

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

/// A0 (ADR-0019): two captures 5 s apart identical with ≥ 99 % lit pixels.
async fn wait_desktop(s: &Arc<Mutex<Screen>>, limit: Duration) -> Option<Duration> {
    let t0 = Instant::now();
    let mut prev: Option<Vec<u8>> = None;
    while t0.elapsed() < limit {
        tokio::time::sleep(Duration::from_secs(5)).await;
        let (w, _, rgb) = snap(s);
        if w == 0 || non_black_permille(&rgb) < 990 {
            prev = None;
            continue;
        }
        if prev.as_deref() == Some(rgb.as_slice()) {
            return Some(t0.elapsed());
        }
        prev = Some(rgb);
    }
    None
}

/// Waits for two captures 1 s apart identical (timeout 20 s, ADR-0019 A4).
async fn wait_stable(s: &Arc<Mutex<Screen>>) -> bool {
    let t0 = Instant::now();
    let mut prev: Option<Vec<u8>> = None;
    while t0.elapsed() < Duration::from_secs(20) {
        tokio::time::sleep(Duration::from_secs(1)).await;
        let (_, _, rgb) = snap(s);
        if prev.as_deref() == Some(rgb.as_slice()) {
            return true;
        }
        prev = Some(rgb);
    }
    false
}

fn check_screen(s: &Arc<Mutex<Screen>>) -> Check {
    let (w, h, rgb) = snap(s);
    Frame {
        width: w,
        height: h,
        rgb: &rgb,
    }
    .check(DEFAULT_CALIBRATION)
}

// ----- keyboard (US layout, ADR-0019) --------------------------------------

fn qcode_for(c: char) -> (bool, &'static str) {
    const LOWER: [&str; 26] = [
        "a", "b", "c", "d", "e", "f", "g", "h", "i", "j", "k", "l", "m", "n", "o", "p", "q", "r",
        "s", "t", "u", "v", "w", "x", "y", "z",
    ];
    const DIGITS: [&str; 10] = ["0", "1", "2", "3", "4", "5", "6", "7", "8", "9"];
    match c {
        'a'..='z' => (false, LOWER[c as usize - 'a' as usize]),
        'A'..='Z' => (true, LOWER[c as usize - 'A' as usize]),
        '0'..='9' => (false, DIGITS[c as usize - '0' as usize]),
        ' ' => (false, "spc"),
        '-' => (false, "minus"),
        '_' => (true, "minus"),
        '/' => (false, "slash"),
        '.' => (false, "dot"),
        ';' => (false, "semicolon"),
        '|' => (true, "backslash"),
        '=' => (false, "equal"),
        '\n' => (false, "ret"),
        other => panic!("character not mapped: {other:?}"),
    }
}

fn key(q: &str, down: bool) -> InputEvent {
    InputEvent::Key(KeyAction {
        qcode: QCode::parse(q).unwrap(),
        down,
    })
}

async fn send_keys(c: &mut Client, vm: VmId, run: RunId, events: Vec<InputEvent>) {
    let st = status(c, vm).await;
    let geometry = lightvm_proto::DisplayGeometry::new(1280, 800).unwrap();
    let _ = st;
    let m = submit(
        c,
        vm,
        Some(run),
        VmAction::Input(InputBatch {
            vm_id: vm,
            run_id: run,
            geometry,
            events,
        }),
    )
    .await;
    assert!(op_ok(&m), "input refused: {m:?}");
}

async fn type_text(c: &mut Client, vm: VmId, run: RunId, text: &str) {
    for ch in text.chars() {
        let (shift, q) = qcode_for(ch);
        let mut ev = Vec::new();
        if shift {
            ev.push(key("shift", true));
        }
        ev.push(key(q, true));
        ev.push(key(q, false));
        if shift {
            ev.push(key("shift", false));
        }
        send_keys(c, vm, run, ev).await;
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

async fn ctrl_alt_t(c: &mut Client, vm: VmId, run: RunId) {
    send_keys(
        c,
        vm,
        run,
        vec![
            key("ctrl", true),
            key("alt", true),
            key("t", true),
            key("t", false),
            key("alt", false),
            key("ctrl", false),
        ],
    )
    .await;
}

/// Fixture command: the data medium is mounted read-only by label if the
/// live session did not mount it already (deviation recorded in F3-01: the
/// automatic mount path of ADR-0019 is not relied upon).
fn fx_command(verb: &str) -> String {
    format!("test -f /mnt/fx.sh || sudo mount -r -L LIGHTVM_FX /mnt; sh /mnt/fx.sh {verb}\n")
}

// ----- service ------------------------------------------------------------

struct Run {
    vm: VmId,
    run: RunId,
    screen: Arc<Mutex<Screen>>,
}

async fn start_vm(c: &mut Client, vm: VmId) -> Run {
    let m = submit(c, vm, None, VmAction::Start).await;
    assert!(op_ok(&m), "start: {m:?}");
    let run = status(c, vm).await.run_id.expect("run");
    let ticket = match req(c, Request::OpenConsole { vm_id: vm }).await {
        ServerMessage::Response {
            result: ResponseBody::Console { ticket },
            ..
        } => ticket,
        other => panic!("{other:?}"),
    };
    Run {
        vm,
        run,
        screen: watch(&ticket),
    }
}

/// Clean shutdown from the guest: `sudo systemctl poweroff -i` (inhibitors
/// of the live session ignored) in the terminal, then Enter
/// every 5 s while waiting (the live session's casper asks to remove the
/// installation medium and press Enter before powering off).
async fn guest_poweroff(c: &mut Client, r: &Run, shot: &Path) -> bool {
    let (vm, run) = (r.vm, r.run);
    // Typed in the terminal left focused by the last fixture command.
    type_text(c, vm, run, "sudo systemctl poweroff -i\n").await;
    let t0 = Instant::now();
    while t0.elapsed() < Duration::from_secs(180) {
        tokio::time::sleep(Duration::from_secs(5)).await;
        let st = status(c, vm).await;
        if st.state == VmState::Stopped {
            return true;
        }
        if st.state == VmState::Running && st.run_id == Some(run) {
            let geometry = lightvm_proto::DisplayGeometry::new(1280, 800).unwrap();
            let _ = submit(
                c,
                vm,
                Some(run),
                VmAction::Input(InputBatch {
                    vm_id: vm,
                    run_id: run,
                    geometry,
                    events: vec![key("ret", true), key("ret", false)],
                }),
            )
            .await;
        }
        save_png(&r.screen, shot);
    }
    false
}

async fn wait_stopped(c: &mut Client, vm: VmId, limit: Duration) -> bool {
    let t0 = Instant::now();
    while t0.elapsed() < limit {
        if status(c, vm).await.state == VmState::Stopped {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    false
}

/// A3 (ADR-0019): Ctrl+Alt+T, terminal open when the screen differs from
/// the one before within 15 s; then the screen must settle (two captures
/// 1 s apart identical) before typing.
async fn open_terminal(c: &mut Client, r: &Run) -> bool {
    let (_, _, before) = snap(&r.screen);
    ctrl_alt_t(c, r.vm, r.run).await;
    let t0 = Instant::now();
    let mut changed = false;
    while t0.elapsed() < Duration::from_secs(15) {
        tokio::time::sleep(Duration::from_millis(250)).await;
        if snap(&r.screen).2 != before {
            changed = true;
            break;
        }
    }
    wait_stable(&r.screen).await;
    changed
}

/// Opens a terminal and runs the fixture verb; returns the verifier outcome
/// (one retry with a new terminal if the output is not found: keystrokes
/// sent while the desktop was still settling).
async fn fixture(c: &mut Client, r: &Run, verb: &str, retries: &mut u32) -> Check {
    let mut out = Check::NotFound("not run".into());
    for attempt in 0..2 {
        if attempt > 0 {
            *retries += 1;
        }
        open_terminal(c, r).await;
        type_text(c, r.vm, r.run, &fx_command(verb)).await;
        tokio::time::sleep(Duration::from_secs(2)).await;
        wait_stable(&r.screen).await;
        out = check_screen(&r.screen);
        if matches!(out, Check::Ok(_)) {
            return out;
        }
    }
    out
}

fn sha256(p: &Path) -> String {
    qemu_backend::probe::sha256_file(p).unwrap()
}

#[test]
#[ignore = "requires approved QEMU, an accelerator and the Ubuntu ISO (LIGHTVM_E2E_QEMU_DIR)"]
fn e2e_real_qemu_f3_baseline_restore_cycles() {
    let lab = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../lab");
    let iso = std::env::var_os("LIGHTVM_E2E_ISO").map_or_else(
        || lab.join("media").join("ubuntu-26.04.1-desktop-amd64.iso"),
        PathBuf::from,
    );
    let cycles: u32 = std::env::var("LIGHTVM_F3_CYCLES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(30);
    let cpu = std::env::var("LIGHTVM_E2E_CPU").unwrap_or_else(|_| "Skylake-Client-v4".into());
    let evidence = std::env::var_os("LIGHTVM_F3_EVIDENCE_DIR").map_or_else(
        || std::env::temp_dir().join("lightvm-f3-evidence"),
        PathBuf::from,
    );
    std::fs::create_dir_all(&evidence).unwrap();
    let dir = temp_dir("f3-real");
    let _cleanup = RemoveCatalog(dir.clone());
    let fx_iso = dir.with_extension("fx.iso");
    std::fs::write(&fx_iso, lightvm_fixture::fixture_iso()).unwrap();
    eprintln!(
        "fixture ISO {} bytes sha256 {}",
        std::fs::metadata(&fx_iso).unwrap().len(),
        sha256(&fx_iso)
    );

    let store = SqliteStore::open(&dir).unwrap();
    let mut setup = QemuSetup::new(qemu_dir(), store.root(), short_dir("lvf"));
    setup.cpu_model = cpu;
    let (host, vm_backend, capacity) = probe(setup).expect("QEMU probe");
    assert!(capacity.accelerator_available && capacity.baseline_available);
    let audit_file = dir.join("audit").join("audit.jsonl");
    let audit = FileAuditSink::open(&audit_file).unwrap();
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
        let data = import(&mut c, &fx_iso, MediaKind::DataIso).await;
        let mut vmc = sample_config("f3-fixture");
        vmc.vcpus = 4;
        vmc.memory_mib = 4096;
        vmc.disk_gib = 8;
        vmc.install_media = Some(install.id);
        vmc.data_media = Some(data.id);
        let vm = match req(&mut c, Request::CreateVm { config: vmc }).await {
            ServerMessage::Response {
                result: ResponseBody::VmCreated { vm },
                ..
            } => vm.id,
            other => panic!("{other:?}"),
        };
        let mut retries = 0u32;
        let mut failures: Vec<String> = Vec::new();

        // ----- first boot: empty disk, reference screenshot, clean shutdown
        let r = start_vm(&mut c, vm).await;
        let boot = wait_desktop(&r.screen, Duration::from_secs(300))
            .await
            .expect("desktop (A0)");
        eprintln!("first boot: desktop after {boot:?}");
        let (w, h, rgb) = snap(&r.screen);
        let reference_clean = !Frame {
            width: w,
            height: h,
            rgb: &rgb,
        }
        .has_fixture_colour();
        save_png(&r.screen, &evidence.join("f3-0-reference.png"));
        assert!(reference_clean, "fixture colours in the reference: inconclusive");
        let first = fixture(&mut c, &r, "mostra", &mut retries).await;
        save_png(&r.screen, &evidence.join("f3-0-mostra.png"));
        assert_eq!(first, Check::Ok(Marker::Absent), "V3 on the empty disk");
        // Clean shutdown from the guest (observed: the baseline is verified).
        assert!(
            guest_poweroff(&mut c, &r, &evidence.join("f3-0-poweroff.png")).await,
            "poweroff"
        );

        let t = Instant::now();
        let base = match req(
            &mut c,
            Request::CreateBaseline {
                vm_id: vm,
                label: Label::parse("fixture empty disk").unwrap(),
                allow_unclean: false,
            },
        )
        .await
        {
            ServerMessage::Response {
                result: ResponseBody::BaselineCreated { baseline },
                ..
            } => baseline,
            other => panic!("{other:?}"),
        };
        eprintln!(
            "baseline {} created in {:?}: clean_shutdown {}, {} bytes, disk sha256 {}",
            base.baseline_id,
            t.elapsed(),
            base.clean_shutdown,
            base.size_bytes,
            base.disk_sha256.to_hex()
        );
        assert!(base.clean_shutdown, "observed guest shutdown: verified baseline");
        let base_file = dir
            .join("vms")
            .join(vm.to_string())
            .join("base")
            .join(base.baseline_id.to_string())
            .join("disk.qcow2");
        let base_vars = base_file.with_file_name("vars.fd");
        let h0 = sha256(&base_file);
        let v0 = sha256(&base_vars);
        assert_eq!(h0, base.disk_sha256.to_hex());

        // ----- cycles ------------------------------------------------------
        let mut boots = Vec::new();
        let mut restores = Vec::new();
        for i in 1..=cycles {
            let t = Instant::now();
            match req(
                &mut c,
                Request::RestoreBaseline {
                    vm_id: vm,
                    baseline_id: base.baseline_id,
                },
            )
            .await
            {
                ServerMessage::Response {
                    result: ResponseBody::BaselineRestored { .. },
                    ..
                } => {}
                other => panic!("cycle {i}: restore: {other:?}"),
            }
            restores.push(t.elapsed());
            let r = start_vm(&mut c, vm).await;
            let Some(b) = wait_desktop(&r.screen, Duration::from_secs(300)).await else {
                save_png(&r.screen, &evidence.join(format!("f3-{i:02}-timeout.png")));
                failures.push(format!("cycle {i}: timed_out (A0)"));
                let _ = submit(&mut c, vm, Some(r.run), VmAction::ForceStop).await;
                wait_stopped(&mut c, vm, Duration::from_secs(60)).await;
                continue;
            };
            boots.push(b);
            let v6 = fixture(&mut c, &r, "mostra", &mut retries).await;
            if i <= 2 {
                save_png(&r.screen, &evidence.join(format!("f3-{i:02}-a-after-restore.png")));
            }
            let v4 = fixture(&mut c, &r, "scrivi", &mut retries).await;
            if i <= 2 {
                save_png(&r.screen, &evidence.join(format!("f3-{i:02}-b-written.png")));
            }
            let ok6 = v6 == Check::Ok(Marker::Absent);
            let ok4 = v4 == Check::Ok(Marker::Present);
            if !ok6 || !ok4 {
                save_png(&r.screen, &evidence.join(format!("f3-{i:02}-failed.png")));
                failures.push(format!("cycle {i}: V6 {v6:?} V4 {v4:?}"));
            }
            // Stop: alternately forced and from the guest.
            let forced = i % 2 == 0;
            if forced {
                let m = submit(&mut c, vm, Some(r.run), VmAction::ForceStop).await;
                assert!(op_ok(&m), "{m:?}");
            } else if !guest_poweroff(&mut c, &r, &evidence.join(format!("f3-{i:02}-poweroff.png")))
                .await
            {
                failures.push(format!("cycle {i}: guest poweroff not completed"));
                let _ = submit(&mut c, vm, Some(r.run), VmAction::ForceStop).await;
            }
            assert!(wait_stopped(&mut c, vm, Duration::from_secs(180)).await, "stop");
            // V5 persistence on some cycles: restart without restore.
            let mut v5 = None;
            if i == 1 || i % 10 == 0 {
                let r = start_vm(&mut c, vm).await;
                if wait_desktop(&r.screen, Duration::from_secs(300)).await.is_some() {
                    let o = fixture(&mut c, &r, "mostra", &mut retries).await;
                    if o != Check::Ok(Marker::Present) {
                        save_png(&r.screen, &evidence.join(format!("f3-{i:02}-v5-failed.png")));
                        failures.push(format!("cycle {i}: V5 {o:?}"));
                    }
                    v5 = Some(o);
                } else {
                    failures.push(format!("cycle {i}: timed_out (A7)"));
                }
                let _ = submit(&mut c, vm, Some(r.run), VmAction::ForceStop).await;
                assert!(wait_stopped(&mut c, vm, Duration::from_secs(60)).await);
            }
            let hi = sha256(&base_file);
            let vi = sha256(&base_vars);
            if hi != h0 || vi != v0 {
                failures.push(format!("cycle {i}: base hash changed"));
            }
            eprintln!(
                "cycle {i}/{cycles}: restore {:?}, desktop {b:?}, V6 {v6:?}, V4 {v4:?}, V5 {v5:?}, stop {}, base unchanged {}, retries so far {retries}",
                restores.last().unwrap(),
                if forced { "forced" } else { "guest poweroff" },
                hi == h0 && vi == v0
            );
        }
        // Final checks: base, catalog, audit.
        let list = match req(&mut c, Request::ListBaselines { vm_id: vm }).await {
            ServerMessage::Response {
                result: ResponseBody::Baselines { baselines, .. },
                ..
            } => baselines,
            other => panic!("{other:?}"),
        };
        let b: &BaselineSummary = &list[0];
        assert_eq!(b.disk_sha256.to_hex(), h0);
        let events: Vec<AuditEvent> = std::fs::read_to_string(&audit_file)
            .unwrap_or_default()
            .lines()
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect();
        let n = |a, o| events.iter().filter(|e| e.action == a && e.outcome == o).count();
        let mut restores_ms: Vec<u128> = restores.iter().map(Duration::as_millis).collect();
        restores_ms.sort_unstable();
        let mut boots_s: Vec<u64> = boots.iter().map(Duration::as_secs).collect();
        boots_s.sort_unstable();
        eprintln!(
            "SUMMARY: cycles {cycles}, failures {}, retries {retries}, base sha256 {h0} unchanged {}, restore ms min/median/max {:?}/{:?}/{:?}, desktop s min/median/max {:?}/{:?}/{:?}, audit baseline_restore succeeded {}, baseline_create succeeded {}",
            failures.len(),
            sha256(&base_file) == h0,
            restores_ms.first(),
            restores_ms.get(restores_ms.len() / 2),
            restores_ms.last(),
            boots_s.first(),
            boots_s.get(boots_s.len() / 2),
            boots_s.last(),
            n(AuditAction::BaselineRestore, AuditOutcome::Succeeded),
            n(AuditAction::BaselineCreate, AuditOutcome::Succeeded),
        );
        for f in &failures {
            eprintln!("FAILURE: {f}");
        }
        handle.shutdown().await;
        assert!(failures.is_empty(), "{failures:?}");
    });
    let _ = std::fs::remove_file(&fx_iso);
}

// ----- real process kill of the service during a real `qemu-img convert` --

/// Child process: full service on the real backend over the catalog in
/// `LIGHTVM_F3_KILL_SERVICE_CHILD`; creates a VM, puts the prepared disk in
/// place and asks for a baseline, then waits to be killed.
fn kill_service_child(dir: &Path) {
    let store = SqliteStore::open(dir).unwrap();
    let setup = QemuSetup::new(qemu_dir(), store.root(), short_dir("lvk"));
    let (host, vm_backend, capacity) = probe(setup).expect("QEMU probe");
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
        let mut cfg = ServiceConfig::new(dir);
        cfg.runtime_dir = Some(dir.join("run"));
        cfg.display_dir = Some(short_dir("lvd"));
        let (handle, ui) = start(cfg, Box::new(engine), Box::new(host)).await.unwrap();
        let mut c = Client::connect(handle.endpoint()).await.unwrap();
        c.hello(&ui).await.unwrap();
        let mut vmc = sample_config("f3-kill");
        vmc.disk_gib = 8;
        let vm = match req(&mut c, Request::CreateVm { config: vmc }).await {
            ServerMessage::Response {
                result: ResponseBody::VmCreated { vm },
                ..
            } => vm.id,
            other => panic!("{other:?}"),
        };
        let vm_dir = dir.join("vms").join(vm.to_string());
        std::fs::create_dir_all(&vm_dir).unwrap();
        std::fs::rename(dir.join("big.qcow2"), vm_dir.join("primary.qcow2")).unwrap();
        std::fs::write(dir.join("vm-id"), vm.to_string()).unwrap();
        // The reply never arrives: the parent kills this process during the
        // convert.
        let m = req(
            &mut c,
            Request::CreateBaseline {
                vm_id: vm,
                label: Label::parse("killed").unwrap(),
                allow_unclean: false,
            },
        )
        .await;
        std::fs::write(dir.join("unexpected-reply"), format!("{m:?}")).unwrap();
    });
}

/// Pseudo-random, incompressible bytes (xorshift), so that the qcow2 has
/// allocated clusters and the convert takes a few seconds.
fn noise(len: usize, seed: u64) -> Vec<u8> {
    let mut x = seed | 1;
    let mut v = Vec::with_capacity(len);
    while v.len() < len {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        v.extend_from_slice(&x.to_le_bytes());
    }
    v.truncate(len);
    v
}

#[test]
#[ignore = "requires approved QEMU (LIGHTVM_E2E_QEMU_DIR); kills a child service process"]
fn e2e_real_kill_service_during_baseline_convert() {
    if let Some(dir) = std::env::var_os("LIGHTVM_F3_KILL_SERVICE_CHILD") {
        kill_service_child(Path::new(&dir));
        return;
    }
    let dir = temp_dir("f3-kill");
    let _cleanup = RemoveCatalog(dir.clone());
    std::fs::create_dir_all(&dir).unwrap();
    // 3 GiB disk with data (raw to qcow2 with the approved qemu-img).
    let raw = dir.join("big.raw");
    {
        use std::io::Write as _;
        let mut f = std::fs::File::create(&raw).unwrap();
        for i in 0..48u64 {
            f.write_all(&noise(64 << 20, i + 1)).unwrap();
        }
    }
    let qemu_img = qemu_dir().join("qemu-img.exe");
    let big = dir.join("big.qcow2");
    let o = qemu_backend::probe::run_capture(
        &qemu_img,
        &[
            "convert",
            "-q",
            "-f",
            "raw",
            "-O",
            "qcow2",
            raw.to_str().unwrap(),
            big.to_str().unwrap(),
        ],
        Duration::from_secs(600),
    )
    .unwrap();
    assert_eq!(o.code, Some(0), "{}", o.stderr);
    std::fs::remove_file(&raw).unwrap();
    let disk_sha = sha256(&big);
    eprintln!(
        "disk {} bytes sha256 {disk_sha}",
        std::fs::metadata(&big).unwrap().len()
    );

    let mut child = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "e2e_real_kill_service_during_baseline_convert",
            "--ignored",
            "--nocapture",
        ])
        .env("LIGHTVM_F3_KILL_SERVICE_CHILD", &dir)
        .spawn()
        .unwrap();
    // Wait for the convert in progress: temporary of the baseline growing.
    let t0 = Instant::now();
    let tmp = dir.join("tmp");
    let partial = loop {
        assert!(t0.elapsed() < Duration::from_secs(180), "convert not seen");
        if let Some(st) = child.try_wait().unwrap() {
            panic!("child exited: {st:?}");
        }
        let found = std::fs::read_dir(&tmp).ok().and_then(|rd| {
            rd.flatten().map(|e| e.path()).find(|p| {
                p.file_name()
                    .is_some_and(|n| n.to_string_lossy().starts_with("promote_baseline-"))
            })
        });
        if let Some(p) = found
            && std::fs::metadata(&p).map_or(0, |m| m.len()) > 256 << 20
        {
            break p;
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    let size_at_kill = std::fs::metadata(&partial).map_or(0, |m| m.len());
    child.kill().unwrap();
    child.wait().unwrap();
    eprintln!(
        "service killed during the convert: partial image {size_at_kill} bytes after {:?}",
        t0.elapsed()
    );
    // qemu-img died with the service (Job Object kill-on-close): the partial
    // file is no longer held open.
    let t1 = Instant::now();
    loop {
        let free = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&partial)
            .is_ok();
        if free {
            break;
        }
        assert!(t1.elapsed() < Duration::from_secs(10), "qemu-img survived");
        std::thread::sleep(Duration::from_millis(50));
    }
    let vm: VmId = std::fs::read_to_string(dir.join("vm-id"))
        .unwrap()
        .parse()
        .unwrap();
    // Reopen (reconciliation at open), then check: no baseline, no
    // temporary, no open intent, disk intact.
    let mut cat = vm_storage::Catalog::open(&dir).unwrap();
    let rep = cat.open_report().unwrap().recovery.clone();
    eprintln!(
        "reconciliation: {:?}",
        rep.intents
            .iter()
            .map(|i| (i.kind, i.outcome, i.detail.clone()))
            .collect::<Vec<_>>()
    );
    assert!(cat.open_intents().unwrap().is_empty());
    assert_eq!(std::fs::read_dir(&tmp).unwrap().count(), 0, "tmp/ empty");
    assert_eq!(cat.baseline_overview(&vm).unwrap().count, 0);
    let vm_dir = dir.join("vms").join(vm.to_string());
    let base_left = std::fs::read_dir(vm_dir.join("base")).map_or(0, |rd| rd.count());
    assert_eq!(base_left, 0, "no baseline directory left");
    assert_eq!(
        sha256(&vm_dir.join("primary.qcow2")),
        disk_sha,
        "disk intact"
    );
    assert!(!dir.join("unexpected-reply").exists());
    drop(cat);

    // The VM is usable: a new baseline through a new service succeeds (with
    // qemu-img compare inside the operation), then a restore.
    let store = SqliteStore::open(&dir).unwrap();
    let setup = QemuSetup::new(qemu_dir(), store.root(), short_dir("lvk"));
    let (host, vm_backend, capacity) = probe(setup).expect("QEMU probe");
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
        let mut c = Client::connect(handle.endpoint()).await.unwrap();
        c.hello(&ui).await.unwrap();
        let t = Instant::now();
        let b = match req(
            &mut c,
            Request::CreateBaseline {
                vm_id: vm,
                label: Label::parse("after the crash").unwrap(),
                allow_unclean: false,
            },
        )
        .await
        {
            ServerMessage::Response {
                result: ResponseBody::BaselineCreated { baseline },
                ..
            } => baseline,
            other => panic!("{other:?}"),
        };
        eprintln!(
            "baseline after the crash in {:?}: {} bytes, sha256 {}",
            t.elapsed(),
            b.size_bytes,
            b.disk_sha256.to_hex()
        );
        let t = Instant::now();
        let m = req(
            &mut c,
            Request::RestoreBaseline {
                vm_id: vm,
                baseline_id: b.baseline_id,
            },
        )
        .await;
        assert!(
            matches!(
                m,
                ServerMessage::Response {
                    result: ResponseBody::BaselineRestored { .. },
                    ..
                }
            ),
            "{m:?}"
        );
        eprintln!(
            "restore of a 3 GiB baseline (full hash verification): {:?}",
            t.elapsed()
        );
        // Start on the overlay: the base is verified again (full SHA-256
        // through the guard handles) before QEMU is launched.
        let t = Instant::now();
        let m = submit(&mut c, vm, None, VmAction::Start).await;
        assert!(op_ok(&m), "{m:?}");
        eprintln!(
            "start on the overlay of a 3 GiB baseline (verification + QEMU start): {:?}",
            t.elapsed()
        );
        let m = submit(&mut c, vm, None, VmAction::ForceStop).await;
        assert!(op_ok(&m), "{m:?}");
        assert!(wait_stopped(&mut c, vm, Duration::from_secs(60)).await);
        handle.shutdown().await;
    });
}

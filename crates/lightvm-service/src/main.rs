//! `lightvm-service` binary: started by the UI as a child process.
//!
//! Contract with the parent (no secrets in the arguments, ROADMAP 9):
//! - structured arguments: `--catalog <dir>` (required),
//!   `--qemu-dir <dir>` (approved QEMU distribution; without it, starting VMs
//!   is rejected with `unsupported`; the folder of the Windows build, or a
//!   Unix prefix such as Homebrew's `/opt/homebrew` with `bin/` and
//!   `share/qemu/`), `--firmware-dir <dir>` (project AArch64 firmware,
//!   ADR-0028; default `<state dir>/firmware`), `--swtpm <file>` (default:
//!   `swtpm` in the QEMU folder), `--approved-hashes <file>` (development
//!   approvals of QEMU, firmware and swtpm hashes; default
//!   `<state dir>/approved-dev.sha256` when present; the state dir is the
//!   one of the default audit log), `--audit-log <file>` (default in the
//!   user profile), `--runtime-dir <dir>` (IPC socket on Unix and private
//!   directories of the QEMU sockets), `--parent-watch stdin|none`,
//!   `--parent-loss-policy kill-vms|shutdown-vms|keep-vms` (default
//!   `kill-vms`) and `--parent-loss-powerdown-s N` (ACPI wait for
//!   `shutdown-vms`, default 10);
//! - stdout: a single JSON line `{"event":"ready",...}` with the endpoint and
//!   the initial UI credential; stdout is an anonymous pipe owned by the
//!   parent, hence a private channel;
//! - stdin: lifeline pipe; EOF = parent terminated, `shutdown` line =
//!   controlled shutdown;
//! - stderr: diagnostics, without secrets.
//!
//! Composition root (ROADMAP 6.1): `vm_core::Core` with the SQLite catalog of
//! `vm-storage`, the `qemu-backend` backend, file audit, system clock and
//! identifiers (see `lightvm_service::adapters`).
//!
//! Exit codes: 0 regular shutdown, 2 wrong usage, 3 catalog already in use by
//! another instance, 4 other startup errors.
//!
//! Local emergency command (F4, ROADMAP 7.3, ADR-0009):
//! `lightvm-service --emergency-stop --catalog <dir>` connects to the
//! emergency endpoint of the service running on that catalog (published in
//! `<catalog>/emergency.endpoint`, same per-user ACL as the IPC endpoint),
//! sends the only verb it accepts and prints the report. Usable when the UI
//! is frozen: no credential, no UI, no supervisor thread involved. Exit
//! codes: 0 completed (end of every process observed), 5 not completed,
//! 6 no running service reachable for the catalog.

use std::io::Write;
use std::path::PathBuf;
use std::process::ExitCode;

use lightvm_proto::ipc::SUPPORTED_PROTOCOL_VERSIONS;
use lightvm_service::adapters::audit::{AUDIT_FILE, FileAuditSink, default_state_dir};
use lightvm_service::adapters::backend::{
    self, QemuSetup, UnavailableBackend, UnavailableHost, unavailable_capacity,
};
use lightvm_service::adapters::catalog::{ManagedMediaInspector, SqliteStore};
use lightvm_service::engine::{CoreEngine, VmEngine, core_config};
use lightvm_service::parent::{self, ParentSignal};
use lightvm_service::server::StartError;
use lightvm_service::supervisor::{Backend, ParentLossPolicy};
use lightvm_service::{ServiceConfig, start};

const USAGE: &str = "usage: lightvm-service --catalog <dir> [--qemu-dir <dir>] \
[--firmware-dir <dir>] [--swtpm <file>] [--approved-hashes <file>] [--audit-log <file>] [--runtime-dir <dir>] [--parent-watch stdin|none] \
[--parent-loss-policy kill-vms|shutdown-vms|keep-vms] [--parent-loss-powerdown-s N]
       lightvm-service --emergency-stop --catalog <dir>";

/// Message for a catalog already in use (same text as the service lock).
const ALREADY_RUNNING: &str = "another instance of lightvm-service is already using this catalog";

#[derive(Debug)]
struct Args {
    catalog: PathBuf,
    qemu_dir: Option<PathBuf>,
    firmware_dir: Option<PathBuf>,
    swtpm: Option<PathBuf>,
    approved_hashes: Option<PathBuf>,
    audit_log: Option<PathBuf>,
    runtime_dir: Option<PathBuf>,
    parent_watch: bool,
    parent_loss_policy: ParentLossPolicy,
    emergency_stop: bool,
}

fn parse_args() -> Result<Args, String> {
    let mut it = std::env::args_os().skip(1);
    let mut catalog = None;
    let mut qemu_dir = None;
    let mut firmware_dir = None;
    let mut swtpm = None;
    let mut approved_hashes = None;
    let mut audit_log = None;
    let mut runtime_dir = None;
    let mut parent_watch = true;
    let mut parent_loss: Option<String> = None;
    let mut powerdown_s: u32 = 10;
    let mut emergency_stop = false;
    while let Some(a) = it.next() {
        let a = a
            .into_string()
            .map_err(|_| "non-UTF-8 argument".to_string())?;
        let mut value = || it.next().ok_or_else(|| format!("{a}: missing value"));
        match a.as_str() {
            "--catalog" => catalog = Some(PathBuf::from(value()?)),
            "--qemu-dir" => qemu_dir = Some(PathBuf::from(value()?)),
            "--firmware-dir" => firmware_dir = Some(PathBuf::from(value()?)),
            "--swtpm" => swtpm = Some(PathBuf::from(value()?)),
            "--approved-hashes" => approved_hashes = Some(PathBuf::from(value()?)),
            "--audit-log" => audit_log = Some(PathBuf::from(value()?)),
            "--runtime-dir" => runtime_dir = Some(PathBuf::from(value()?)),
            "--parent-watch" => {
                parent_watch = match value()?.to_str() {
                    Some("stdin") => true,
                    Some("none") => false,
                    _ => return Err("--parent-watch: expected stdin or none".into()),
                }
            }
            "--parent-loss-policy" => {
                parent_loss = Some(
                    value()?
                        .into_string()
                        .map_err(|_| "--parent-loss-policy: non-UTF-8 value".to_string())?,
                );
            }
            "--parent-loss-powerdown-s" => {
                powerdown_s = value()?
                    .to_str()
                    .and_then(|v| v.parse().ok())
                    .ok_or("--parent-loss-powerdown-s: expected a number of seconds")?;
            }
            "--emergency-stop" => emergency_stop = true,
            "--version" => {
                println!("lightvm-service {}", env!("CARGO_PKG_VERSION"));
                std::process::exit(0);
            }
            "--help" | "-h" => {
                println!("{USAGE}");
                std::process::exit(0);
            }
            other => return Err(format!("unknown argument: {other}")),
        }
    }
    let catalog = catalog.ok_or("--catalog is required")?;
    let parent_loss_policy = match parent_loss.as_deref() {
        None | Some("kill-vms") => ParentLossPolicy::KillVms,
        Some("shutdown-vms") => ParentLossPolicy::ShutdownVms { powerdown_s },
        Some("keep-vms") => ParentLossPolicy::KeepVms,
        Some(_) => {
            return Err("--parent-loss-policy: expected kill-vms, shutdown-vms or keep-vms".into());
        }
    }
    .validate()
    .map_err(|e| format!("--parent-loss-powerdown-s: {}", e.message))?;
    Ok(Args {
        catalog,
        qemu_dir,
        firmware_dir,
        swtpm,
        approved_hashes,
        audit_log,
        runtime_dir,
        parent_watch,
        parent_loss_policy,
        emergency_stop,
    })
}

/// `--emergency-stop`: local emergency command, never opens the catalog.
fn emergency_command(catalog: &std::path::Path) -> ExitCode {
    let endpoint = match lightvm_service::client::read_emergency_endpoint(catalog) {
        Ok(e) => e,
        Err(e) => {
            lightvm_service::diag!(
                "lightvm-service: no running service for this catalog (emergency endpoint: {e})"
            );
            return ExitCode::from(6);
        }
    };
    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            lightvm_service::diag!("lightvm-service: runtime: {e}");
            return ExitCode::from(4);
        }
    };
    match rt.block_on(lightvm_service::client::emergency_stop(&endpoint)) {
        Ok(r) => {
            println!(
                "{}",
                serde_json::json!({
                    "event": "emergency_stop",
                    "revoked_connections": r.revoked_connections,
                    "processes_terminated": r.processes_terminated,
                    "completed": r.completed,
                })
            );
            if r.completed {
                ExitCode::SUCCESS
            } else {
                ExitCode::from(5)
            }
        }
        Err(e) => {
            lightvm_service::diag!("lightvm-service: emergency endpoint not reachable: {e}");
            ExitCode::from(6)
        }
    }
}

/// Engine and host composed from the real adapters, and the root of the
/// per-run display sockets.
type Composed = (Box<dyn VmEngine>, Box<dyn Backend>, PathBuf);

/// Synchronous composition, outside the service tokio runtime: the QEMU probe
/// uses the backend's own runtime.
/// Space reserved under the private root for the sockets: separator,
/// per-run subdirectory (12 characters), separator and `qmp.sock`/`vnc.sock`;
/// for the display proxy `\d\<12 characters>\d.sock` (same length).
const SOCKET_RESERVE: usize = 22;

/// Root of the private directories of the QEMU sockets: `--runtime-dir` or
/// `<catalog>/p`. If the path is too long for the sockets (107 bytes,
/// `UNIX_SOCKET_PATH_MAX`) a short directory in the user's temporary folder
/// is used, with a notice, instead of a probe that fails.
fn private_root(
    runtime_dir: Option<&std::path::Path>,
    root: &std::path::Path,
) -> Result<PathBuf, (u8, String)> {
    let max = qemu_backend::validate::UNIX_SOCKET_PATH_MAX;
    let fits = |p: &std::path::Path| p.as_os_str().len() + SOCKET_RESERVE <= max;
    let candidate = runtime_dir.map_or_else(|| root.join("p"), std::path::Path::to_path_buf);
    if fits(&candidate) {
        return Ok(candidate);
    }
    let short = lightvm_service::display::fallback_root();
    if !fits(&short) {
        return Err((
            4,
            "catalog and temporary folder paths too long for the QEMU sockets: choose a catalog with a shorter path".into(),
        ));
    }
    lightvm_service::diag!(
        "lightvm-service: path {} too long for the QEMU sockets, private directory {}",
        candidate.display(),
        short.display()
    );
    Ok(short)
}

/// Firmware directory, swtpm and development approvals (ADR-0028): from the
/// arguments, else the defaults under the user state directory. An
/// approvals file given explicitly must exist and be valid; the default one
/// is used only when present (and must then be valid too).
fn configure_aarch64_and_approvals(setup: &mut QemuSetup, args: &Args) -> Result<(), (u8, String)> {
    let state = default_state_dir();
    setup.firmware_dir = args
        .firmware_dir
        .clone()
        .or_else(|| state.as_ref().map(|d| d.join("firmware")));
    setup.swtpm.clone_from(&args.swtpm);
    let approvals = match (&args.approved_hashes, &state) {
        (Some(p), _) => Some(p.clone()),
        (None, Some(d)) => Some(d.join("approved-dev.sha256")).filter(|p| p.is_file()),
        (None, None) => None,
    };
    if let Some(p) = approvals {
        let text = std::fs::read_to_string(&p)
            .map_err(|e| (4, format!("approved hashes {}: {e}", p.display())))?;
        let n = backend::load_approvals(&mut setup.probe, &text)
            .map_err(|e| (4, format!("approved hashes {}: {e}", p.display())))?;
        lightvm_service::diag!(
            "lightvm-service: {n} development hash approvals from {} (not approved for release)",
            p.display()
        );
    }
    Ok(())
}

fn compose(args: &Args) -> Result<Composed, (u8, String)> {
    let store = match SqliteStore::open(&args.catalog) {
        Ok(s) => s,
        Err(vm_storage::StorageError::CatalogLocked(p)) => {
            return Err((3, format!("{ALREADY_RUNNING} (lock: {})", p.display())));
        }
        Err(e) => return Err((4, format!("catalog: {e}"))),
    };
    let root = store.root();
    let audit_path = match &args.audit_log {
        Some(p) => p.clone(),
        None => default_state_dir()
            .ok_or((
                4,
                "user profile state directory cannot be determined".to_string(),
            ))?
            .join(AUDIT_FILE),
    };
    let audit = FileAuditSink::open(&audit_path)
        .map_err(|e| (4, format!("audit {}: {e}", audit_path.display())))?;
    let inspector = ManagedMediaInspector::new(root.clone());
    // Fallback private roots of services that are no longer running
    // (sockets and display directories left by a crash).
    let stale = lightvm_service::display::remove_stale_fallback_roots(&std::env::temp_dir());
    if stale > 0 {
        lightvm_service::diag!("lightvm-service: {stale} stale private directories removed");
    }
    let private_root = private_root(args.runtime_dir.as_deref(), &root)?;
    let display_dir = private_root.join("d");
    match &args.qemu_dir {
        Some(dir) => {
            let mut setup = QemuSetup::new(dir.clone(), root, private_root);
            configure_aarch64_and_approvals(&mut setup, args)?;
            let (host, vm_backend, capacity) =
                backend::probe(setup).map_err(|e| (4, format!("backend QEMU: {e}")))?;
            let engine =
                CoreEngine::new(store, audit, vm_backend, inspector, core_config(capacity))
                    .map_err(|e| (4, format!("core: {e}")))?;
            Ok((Box::new(engine), Box::new(host), display_dir))
        }
        None => {
            lightvm_service::diag!(
                "lightvm-service: --qemu-dir missing: starting VMs not available"
            );
            let capacity = unavailable_capacity(256);
            let engine = CoreEngine::new(
                store,
                audit,
                UnavailableBackend,
                inspector,
                core_config(capacity),
            )
            .map_err(|e| (4, format!("core: {e}")))?;
            Ok((Box::new(engine), Box::new(UnavailableHost), display_dir))
        }
    }
}

fn main() -> ExitCode {
    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            lightvm_service::diag!("lightvm-service: {e}\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    if args.emergency_stop {
        return emergency_command(&args.catalog);
    }
    let (engine, host, display_dir) = match compose(&args) {
        Ok(x) => x,
        Err((code, msg)) => {
            lightvm_service::diag!("lightvm-service: {msg}");
            return ExitCode::from(code);
        }
    };
    let rt = match tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            lightvm_service::diag!("lightvm-service: runtime: {e}");
            return ExitCode::from(4);
        }
    };
    rt.block_on(run(args, engine, host, display_dir))
}

async fn run(
    args: Args,
    engine: Box<dyn VmEngine>,
    host: Box<dyn Backend>,
    display_dir: PathBuf,
) -> ExitCode {
    let mut cfg = ServiceConfig::new(&args.catalog);
    cfg.runtime_dir = args.runtime_dir;
    cfg.parent_loss_policy = args.parent_loss_policy;
    cfg.display_dir = Some(display_dir);
    let (handle, ui_credential) = match start(cfg, engine, host).await {
        Ok(x) => x,
        Err(e @ StartError::Lock(lightvm_service::lock::LockError::AlreadyRunning { .. })) => {
            lightvm_service::diag!("lightvm-service: {e}");
            return ExitCode::from(3);
        }
        Err(e) => {
            lightvm_service::diag!("lightvm-service: {e}");
            return ExitCode::from(4);
        }
    };

    let ready = serde_json::json!({
        "event": "ready",
        "pid": std::process::id(),
        "endpoint": handle.endpoint(),
        "protocol_versions": SUPPORTED_PROTOCOL_VERSIONS,
        "credential": ui_credential.to_hex(),
    });
    drop(ui_credential);
    {
        let mut out = std::io::stdout().lock();
        if writeln!(out, "{ready}").and_then(|_| out.flush()).is_err() {
            lightvm_service::diag!("lightvm-service: stdout not available, shutting down");
            handle.shutdown().await;
            return ExitCode::from(4);
        }
    }
    drop(ready);

    if args.parent_watch {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<ParentSignal>();
        let tx2 = tx.clone();
        if let Err(e) = parent::watch_stdin(move |s| {
            let _ = tx.send(s);
        }) {
            lightvm_service::diag!("lightvm-service: stdin watch not started: {e}");
            handle.shutdown().await;
            return ExitCode::from(4);
        }
        #[cfg(target_os = "linux")]
        if let Err(e) = parent::watch_parent_pidfd(move |s| {
            let _ = tx2.send(s);
        }) {
            lightvm_service::diag!(
                "lightvm-service: parent pidfd not available ({e}); the lifeline pipe remains"
            );
        }
        #[cfg(not(target_os = "linux"))]
        drop(tx2);
        let mut lost_handled = false;
        loop {
            tokio::select! {
                _ = handle.wait() => break,
                s = rx.recv() => match s {
                    Some(ParentSignal::Shutdown) => {
                        // The EOF that follows the shutdown request is not a
                        // loss of the UI.
                        lost_handled = true;
                        handle.trigger_shutdown();
                    }
                    Some(ParentSignal::Lost) if lost_handled => {}
                    Some(ParentSignal::Lost) => {
                        lost_handled = true;
                        lightvm_service::diag!("lightvm-service: parent process terminated");
                        handle.parent_lost().await;
                    }
                    None => {
                        handle.wait().await;
                        break;
                    }
                },
            }
        }
    } else {
        handle.wait().await;
    }
    lightvm_service::diag!("lightvm-service: shutdown completed");
    ExitCode::SUCCESS
}

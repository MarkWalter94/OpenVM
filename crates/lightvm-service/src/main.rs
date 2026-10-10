//! Binario `lightvm-service`: avviato dalla UI come processo figlio.
//!
//! Contratto con il genitore (nessun segreto negli argomenti, ROADMAP 9):
//! - argomenti strutturati: `--catalog <dir>` (obbligatorio),
//!   `--qemu-dir <dir>` (distribuzione QEMU approvata; senza, l'avvio delle VM
//!   è rifiutato con `unsupported`), `--audit-log <file>` (predefinito nel
//!   profilo utente), `--runtime-dir <dir>` (socket IPC su Unix e directory
//!   private dei socket QEMU), `--parent-watch stdin|none`,
//!   `--parent-loss-policy kill-vms|shutdown-vms|keep-vms` (predefinita
//!   `kill-vms`) e `--parent-loss-powerdown-s N` (attesa ACPI per
//!   `shutdown-vms`, predefinita 10);
//! - stdout: una sola riga JSON `{"event":"ready",...}` con endpoint e
//!   credenziale iniziale della UI; stdout è una pipe anonima posseduta dal
//!   genitore, quindi un canale privato;
//! - stdin: pipe di vita; EOF = genitore terminato, riga `shutdown` = arresto
//!   controllato;
//! - stderr: diagnostica, senza segreti.
//!
//! Composition root (ROADMAP 6.1): `vm_core::Core` con catalogo SQLite di
//! `vm-storage`, backend `qemu-backend`, audit su file, orologio e
//! identificativi di sistema (vedi `lightvm_service::adapters`).
//!
//! Codici di uscita: 0 arresto regolare, 2 uso errato, 3 catalogo già in uso da
//! un'altra istanza, 4 altri errori di avvio.

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

const USAGE: &str = "uso: lightvm-service --catalog <dir> [--qemu-dir <dir>] \
[--audit-log <file>] [--runtime-dir <dir>] [--parent-watch stdin|none] \
[--parent-loss-policy kill-vms|shutdown-vms|keep-vms] [--parent-loss-powerdown-s N]";

/// Messaggio per un catalogo già in uso (stesso testo del lock del servizio).
const ALREADY_RUNNING: &str = "un'altra istanza di lightvm-service sta già usando questo catalogo";

#[derive(Debug)]
struct Args {
    catalog: PathBuf,
    qemu_dir: Option<PathBuf>,
    audit_log: Option<PathBuf>,
    runtime_dir: Option<PathBuf>,
    parent_watch: bool,
    parent_loss_policy: ParentLossPolicy,
}

fn parse_args() -> Result<Args, String> {
    let mut it = std::env::args_os().skip(1);
    let mut catalog = None;
    let mut qemu_dir = None;
    let mut audit_log = None;
    let mut runtime_dir = None;
    let mut parent_watch = true;
    let mut parent_loss: Option<String> = None;
    let mut powerdown_s: u32 = 10;
    while let Some(a) = it.next() {
        let a = a
            .into_string()
            .map_err(|_| "argomento non UTF-8".to_string())?;
        let mut value = || it.next().ok_or_else(|| format!("{a}: valore mancante"));
        match a.as_str() {
            "--catalog" => catalog = Some(PathBuf::from(value()?)),
            "--qemu-dir" => qemu_dir = Some(PathBuf::from(value()?)),
            "--audit-log" => audit_log = Some(PathBuf::from(value()?)),
            "--runtime-dir" => runtime_dir = Some(PathBuf::from(value()?)),
            "--parent-watch" => {
                parent_watch = match value()?.to_str() {
                    Some("stdin") => true,
                    Some("none") => false,
                    _ => return Err("--parent-watch: atteso stdin o none".into()),
                }
            }
            "--parent-loss-policy" => {
                parent_loss = Some(
                    value()?
                        .into_string()
                        .map_err(|_| "--parent-loss-policy: valore non UTF-8".to_string())?,
                );
            }
            "--parent-loss-powerdown-s" => {
                powerdown_s = value()?
                    .to_str()
                    .and_then(|v| v.parse().ok())
                    .ok_or("--parent-loss-powerdown-s: atteso un numero di secondi")?;
            }
            "--version" => {
                println!("lightvm-service {}", env!("CARGO_PKG_VERSION"));
                std::process::exit(0);
            }
            "--help" | "-h" => {
                println!("{USAGE}");
                std::process::exit(0);
            }
            other => return Err(format!("argomento sconosciuto: {other}")),
        }
    }
    let catalog = catalog.ok_or("--catalog obbligatorio")?;
    let parent_loss_policy = match parent_loss.as_deref() {
        None | Some("kill-vms") => ParentLossPolicy::KillVms,
        Some("shutdown-vms") => ParentLossPolicy::ShutdownVms { powerdown_s },
        Some("keep-vms") => ParentLossPolicy::KeepVms,
        Some(_) => {
            return Err("--parent-loss-policy: atteso kill-vms, shutdown-vms o keep-vms".into());
        }
    }
    .validate()
    .map_err(|e| format!("--parent-loss-powerdown-s: {}", e.message))?;
    Ok(Args {
        catalog,
        qemu_dir,
        audit_log,
        runtime_dir,
        parent_watch,
        parent_loss_policy,
    })
}

/// Motore e host composti dagli adattatori reali.
type Composed = (Box<dyn VmEngine>, Box<dyn Backend>);

/// Composizione sincrona, fuori dal runtime tokio del servizio: il probe di
/// QEMU usa il runtime proprio del backend.
/// Spazio riservato sotto la radice privata per i socket di QEMU:
/// separatore, sottodirectory per esecuzione (12 caratteri), separatore e
/// `qmp.sock`/`vnc.sock`.
const RISERVA_SOCKET: usize = 22;

/// Radice delle directory private dei socket QEMU: `--runtime-dir` oppure
/// `<catalogo>/p`. Se il percorso è troppo lungo per i socket (107 byte,
/// `UNIX_SOCKET_PATH_MAX`) si usa una directory corta nella cartella
/// temporanea dell'utente, segnalandolo, invece di un probe che fallisce.
fn private_root(
    runtime_dir: Option<&std::path::Path>,
    root: &std::path::Path,
) -> Result<PathBuf, (u8, String)> {
    let max = qemu_backend::validate::UNIX_SOCKET_PATH_MAX;
    let entra = |p: &std::path::Path| p.as_os_str().len() + RISERVA_SOCKET <= max;
    let candidato = runtime_dir.map_or_else(|| root.join("p"), std::path::Path::to_path_buf);
    if entra(&candidato) {
        return Ok(candidato);
    }
    let corta = std::env::temp_dir().join(format!("lightvm-p-{}", std::process::id()));
    if !entra(&corta) {
        return Err((
            4,
            "percorsi del catalogo e della cartella temporanea troppo lunghi per i socket di QEMU: scegliere un catalogo con un percorso più corto".into(),
        ));
    }
    lightvm_service::diag!(
        "lightvm-service: percorso {} troppo lungo per i socket di QEMU, directory privata {}",
        candidato.display(),
        corta.display()
    );
    Ok(corta)
}

fn compose(args: &Args) -> Result<Composed, (u8, String)> {
    let store = match SqliteStore::open(&args.catalog) {
        Ok(s) => s,
        Err(vm_storage::StorageError::CatalogLocked(p)) => {
            return Err((3, format!("{ALREADY_RUNNING} (lock: {})", p.display())));
        }
        Err(e) => return Err((4, format!("catalogo: {e}"))),
    };
    let root = store.root();
    let audit_path = match &args.audit_log {
        Some(p) => p.clone(),
        None => default_state_dir()
            .ok_or((
                4,
                "directory di stato del profilo utente non determinabile".to_string(),
            ))?
            .join(AUDIT_FILE),
    };
    let audit = FileAuditSink::open(&audit_path)
        .map_err(|e| (4, format!("audit {}: {e}", audit_path.display())))?;
    let inspector = ManagedMediaInspector::new(root.clone());
    match &args.qemu_dir {
        Some(dir) => {
            let private_root = private_root(args.runtime_dir.as_deref(), &root)?;
            let setup = QemuSetup::new(dir.clone(), root, private_root);
            let (host, vm_backend, capacity) =
                backend::probe(setup).map_err(|e| (4, format!("backend QEMU: {e}")))?;
            let engine =
                CoreEngine::new(store, audit, vm_backend, inspector, core_config(capacity))
                    .map_err(|e| (4, format!("core: {e}")))?;
            Ok((Box::new(engine), Box::new(host)))
        }
        None => {
            lightvm_service::diag!(
                "lightvm-service: --qemu-dir assente: avvio delle VM non disponibile"
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
            Ok((Box::new(engine), Box::new(UnavailableHost)))
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
    let (engine, host) = match compose(&args) {
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
    rt.block_on(run(args, engine, host))
}

async fn run(args: Args, engine: Box<dyn VmEngine>, host: Box<dyn Backend>) -> ExitCode {
    let mut cfg = ServiceConfig::new(&args.catalog);
    cfg.runtime_dir = args.runtime_dir;
    cfg.parent_loss_policy = args.parent_loss_policy;
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
            lightvm_service::diag!("lightvm-service: stdout non disponibile, arresto");
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
            lightvm_service::diag!("lightvm-service: sorveglianza stdin non avviata: {e}");
            handle.shutdown().await;
            return ExitCode::from(4);
        }
        #[cfg(target_os = "linux")]
        if let Err(e) = parent::watch_parent_pidfd(move |s| {
            let _ = tx2.send(s);
        }) {
            lightvm_service::diag!(
                "lightvm-service: pidfd del genitore non disponibile ({e}); resta la pipe di vita"
            );
        }
        #[cfg(not(target_os = "linux"))]
        drop(tx2);
        let mut lost_handled = false;
        loop {
            tokio::select! {
                _ = handle.wait() => break,
                s = rx.recv() => match s {
                    Some(ParentSignal::Shutdown) => handle.trigger_shutdown(),
                    Some(ParentSignal::Lost) if lost_handled => {}
                    Some(ParentSignal::Lost) => {
                        lost_handled = true;
                        lightvm_service::diag!("lightvm-service: processo genitore terminato");
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
    lightvm_service::diag!("lightvm-service: arresto completato");
    ExitCode::SUCCESS
}

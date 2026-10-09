//! Binario `lightvm-service`: avviato dalla UI come processo figlio.
//!
//! Contratto con il genitore (nessun segreto negli argomenti, ROADMAP 9):
//! - argomenti strutturati: `--catalog <dir>` (obbligatorio),
//!   `--runtime-dir <dir>` (solo Unix), `--parent-watch stdin|none`,
//!   `--orphan-policy shutdown|keep-manual-vms`;
//! - stdout: una sola riga JSON `{"event":"ready",...}` con endpoint e
//!   credenziale iniziale della UI; stdout è una pipe anonima posseduta dal
//!   genitore, quindi un canale privato;
//! - stdin: pipe di vita; EOF = genitore terminato, riga `shutdown` = arresto
//!   controllato;
//! - stderr: diagnostica, senza segreti.
//!
//! Codici di uscita: 0 arresto regolare, 2 uso errato, 3 catalogo già in uso da
//! un'altra istanza, 4 altri errori di avvio.

use std::io::Write;
use std::path::PathBuf;
use std::process::ExitCode;

use lightvm_service::compat::SUPPORTED_PROTOCOL_VERSIONS;
use lightvm_service::parent::{self, ParentSignal};
use lightvm_service::server::StartError;
use lightvm_service::supervisor::OrphanPolicy;
use lightvm_service::supervisor::fake::{FakeBackend, FakeCatalog};
use lightvm_service::{ServiceConfig, start};

const USAGE: &str = "uso: lightvm-service --catalog <dir> [--runtime-dir <dir>] \
[--parent-watch stdin|none] [--orphan-policy shutdown|keep-manual-vms]";

#[derive(Debug)]
struct Args {
    catalog: PathBuf,
    runtime_dir: Option<PathBuf>,
    parent_watch: bool,
    orphan_policy: OrphanPolicy,
}

fn parse_args() -> Result<Args, String> {
    let mut it = std::env::args_os().skip(1);
    let mut catalog = None;
    let mut runtime_dir = None;
    let mut parent_watch = true;
    let mut orphan_policy = OrphanPolicy::ShutdownService;
    while let Some(a) = it.next() {
        let a = a
            .into_string()
            .map_err(|_| "argomento non UTF-8".to_string())?;
        let mut value = || it.next().ok_or_else(|| format!("{a}: valore mancante"));
        match a.as_str() {
            "--catalog" => catalog = Some(PathBuf::from(value()?)),
            "--runtime-dir" => runtime_dir = Some(PathBuf::from(value()?)),
            "--parent-watch" => {
                parent_watch = match value()?.to_str() {
                    Some("stdin") => true,
                    Some("none") => false,
                    _ => return Err("--parent-watch: atteso stdin o none".into()),
                }
            }
            "--orphan-policy" => {
                orphan_policy = match value()?.to_str() {
                    Some("shutdown") => OrphanPolicy::ShutdownService,
                    Some("keep-manual-vms") => OrphanPolicy::KeepManualVms,
                    _ => return Err("--orphan-policy: atteso shutdown o keep-manual-vms".into()),
                }
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
    Ok(Args {
        catalog,
        runtime_dir,
        parent_watch,
        orphan_policy,
    })
}

fn main() -> ExitCode {
    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            lightvm_service::diag!("lightvm-service: {e}\n{USAGE}");
            return ExitCode::from(2);
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
    rt.block_on(run(args))
}

async fn run(args: Args) -> ExitCode {
    let mut cfg = ServiceConfig::new(&args.catalog);
    cfg.runtime_dir = args.runtime_dir;
    cfg.orphan_policy = args.orphan_policy;
    // TODO(F1): sostituire con vm-storage e qemu-backend.
    let (handle, ui_credential) = match start(
        cfg,
        Box::new(FakeCatalog::sample()),
        Box::<FakeBackend>::default(),
    )
    .await
    {
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

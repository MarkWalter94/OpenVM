//! Supervisor come processo figlio della UI (F1-11, ADR-0002; presupposto di
//! P-2 in `docs/STATUS.md`).
//!
//! La UI avvia `lightvm-service` con argomenti strutturati e tiene tre pipe
//! anonime (contratto in `crates/lightvm-service/src/main.rs`):
//! - stdout: una sola riga JSON `ready` con endpoint IPC e credenziale UI;
//! - stdin: pipe di vita (EOF = UI terminata) e riga `shutdown` per l'arresto
//!   controllato;
//! - stderr: ereditato, diagnostica senza segreti.
//!
//! Dopo la riga `ready` la UI parla col servizio solo via IPC autenticato, con
//! due connessioni: una per le richieste (risposta per `request_id`) e una per
//! gli eventi (`subscribe`). Le letture di un frame non sono interrompibili in
//! sicurezza, quindi richieste ed eventi non condividono la connessione.
//!
//! La UI non apre QMP né file VM: avvio, arresto ed Emergency Stop passano dal
//! core del servizio (ROADMAP 6.1).

use std::io::{BufRead, BufReader, Write as _};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use lightvm_proto::ipc::{
    ParentLossPolicy, Request, ResponseBody, ServerMessage, ServiceEvent, VmSummary,
};
use lightvm_proto::{
    OperationId, OperationStatus, ReplyError, VmAction, VmConfig, VmId, VmRequest,
};
use lightvm_service::client::Client;
use lightvm_service::credential::Credential;
use tokio::sync::mpsc;

/// Attesa massima della riga `ready`.
pub const ATTESA_PRONTO: Duration = Duration::from_secs(30);
/// Attesa massima dell'uscita dopo la richiesta di arresto controllato:
/// oltre il timeout di arresto del servizio (30 s, poi Emergency Stop delle VM
/// rimaste), così la UI non lo termina a metà dell'arresto ordinato.
pub const ATTESA_ARRESTO: Duration = Duration::from_secs(45);

/// Parametri di avvio del supervisor.
#[derive(Clone, Debug)]
pub struct ConfigServizio {
    /// Eseguibile `lightvm-service`.
    pub eseguibile: PathBuf,
    /// Directory del catalogo.
    pub catalogo: PathBuf,
    /// Distribuzione QEMU approvata; senza, il servizio rifiuta gli avvii.
    pub qemu_dir: Option<PathBuf>,
    /// Directory runtime (socket IPC su Unix, socket QEMU privati).
    pub runtime_dir: Option<PathBuf>,
    /// File di audit; senza, il servizio usa il profilo utente
    /// (`%LOCALAPPDATA%\LightVM\audit.jsonl` su Windows).
    pub audit_log: Option<PathBuf>,
    /// Sorte delle VM se la UI termina senza arresto controllato.
    pub politica: ParentLossPolicy,
}

impl ConfigServizio {
    /// Eseguibile accanto a quello della UI (`lightvm-service[.exe]`).
    pub fn eseguibile_accanto() -> Option<PathBuf> {
        let exe = std::env::current_exe().ok()?;
        Some(exe.with_file_name(format!("lightvm-service{}", std::env::consts::EXE_SUFFIX)))
    }

    fn comando(&self) -> Command {
        let mut c = Command::new(&self.eseguibile);
        c.arg("--catalog").arg(&self.catalogo);
        if let Some(q) = &self.qemu_dir {
            c.arg("--qemu-dir").arg(q);
        }
        if let Some(r) = &self.runtime_dir {
            c.arg("--runtime-dir").arg(r);
        }
        if let Some(a) = &self.audit_log {
            c.arg("--audit-log").arg(a);
        }
        match self.politica {
            ParentLossPolicy::KillVms => {
                c.args(["--parent-loss-policy", "kill-vms"]);
            }
            ParentLossPolicy::ShutdownVms { powerdown_s } => {
                c.args(["--parent-loss-policy", "shutdown-vms"])
                    .arg("--parent-loss-powerdown-s")
                    .arg(powerdown_s.to_string());
            }
            ParentLossPolicy::KeepVms => {
                c.args(["--parent-loss-policy", "keep-vms"]);
            }
        }
        c.args(["--parent-watch", "stdin"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        c
    }
}

/// Stato iniziale della connessione, prima dell'handshake.
pub const CONNESSIONE_IN_CORSO: &str = "connessione in corso";

/// Stato visto dalla UI, aggiornato dai thread IPC.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StatoServizio {
    /// Stato della connessione, leggibile.
    pub connessione: String,
    /// Connessione stabilita e handshake riuscito.
    pub connesso: bool,
    /// Backend riportato dal servizio.
    pub backend: Option<String>,
    /// Acceleratore riportato dal servizio.
    pub acceleratore: Option<String>,
    /// VM del catalogo.
    pub vms: Vec<VmSummary>,
    /// Esito dell'ultima azione richiesta, leggibile.
    pub ultimo_esito: Option<String>,
    /// Politica alla perdita della UI riportata dal servizio.
    pub politica: Option<ParentLossPolicy>,
}

/// Azioni richieste dalla UI al servizio.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Comando {
    /// Rilegge elenco VM e capacità.
    Aggiorna,
    /// Crea una VM nel catalogo del servizio. Non esposto nei widget: la
    /// creazione guidata arriva in F2; serve alle prove (F1-12, P-2).
    Crea(Box<VmConfig>),
    /// Avvia una VM.
    Avvia(VmId),
    /// Arresto ordinario di una VM.
    Arresta(VmId),
    /// Emergency Stop (percorso prioritario del servizio).
    EmergencyStop,
    /// Cambia la sorte delle VM alla perdita della UI (vale subito).
    ImpostaPolitica(ParentLossPolicy),
    /// Salva risorse e limiti di una VM spenta.
    AggiornaVm {
        /// VM.
        vm_id: VmId,
        /// Revisione su cui si basa la modifica.
        revisione: u64,
        /// Nuova configurazione.
        config: Box<VmConfig>,
    },
    /// Apre una VM per nome (riga di comando): applica le modifiche
    /// richieste se la VM è spenta, poi la avvia.
    ApriVm {
        /// Nome della VM.
        nome: String,
        /// Modifiche a risorse e limiti.
        modifiche: crate::impostazioni_vm::ModificheVm,
        /// Se la VM non esiste, crearla con i valori predefiniti più le
        /// modifiche (senza supporto di installazione: l'importazione dei
        /// media arriva con la creazione guidata, F2).
        crea: bool,
    },
    /// Apre il pannello delle impostazioni della VM: gestito dalla UI, mai
    /// inviato al supervisor.
    ApriImpostazioni(VmId),
}

/// Errori di avvio del supervisor.
#[derive(Debug)]
pub enum ErroreAvvio {
    /// Processo non avviato.
    Spawn(std::io::Error),
    /// Il servizio è uscito prima della riga `ready` (codice di uscita, se noto).
    UscitoPrimaDelPronto(Option<i32>),
    /// Nessuna riga `ready` entro [`ATTESA_PRONTO`].
    Timeout,
    /// Riga `ready` non valida.
    RigaNonValida(String),
}

impl std::fmt::Display for ErroreAvvio {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Spawn(e) => write!(f, "avvio di lightvm-service fallito: {e}"),
            Self::UscitoPrimaDelPronto(Some(3)) => {
                write!(
                    f,
                    "il catalogo è già usato da un'altra istanza di lightvm-service"
                )
            }
            Self::UscitoPrimaDelPronto(c) => {
                write!(
                    f,
                    "lightvm-service è uscito prima di essere pronto (codice {c:?})"
                )
            }
            Self::Timeout => write!(f, "lightvm-service non ha segnalato di essere pronto"),
            Self::RigaNonValida(m) => write!(f, "riga di avvio di lightvm-service non valida: {m}"),
        }
    }
}

impl std::error::Error for ErroreAvvio {}

/// Dati della riga `ready` (la credenziale non è mai registrata).
struct Pronto {
    pid: u32,
    endpoint: String,
    credenziale: Credential,
}

fn leggi_pronto(riga: &str) -> Result<Pronto, ErroreAvvio> {
    let v: serde_json::Value =
        serde_json::from_str(riga).map_err(|e| ErroreAvvio::RigaNonValida(e.to_string()))?;
    if v.get("event").and_then(|e| e.as_str()) != Some("ready") {
        return Err(ErroreAvvio::RigaNonValida("evento diverso da ready".into()));
    }
    let pid = v
        .get("pid")
        .and_then(serde_json::Value::as_u64)
        .and_then(|p| u32::try_from(p).ok())
        .ok_or_else(|| ErroreAvvio::RigaNonValida("pid mancante".into()))?;
    let endpoint = v
        .get("endpoint")
        .and_then(|e| e.as_str())
        .ok_or_else(|| ErroreAvvio::RigaNonValida("endpoint mancante".into()))?
        .to_owned();
    let credenziale = v
        .get("credential")
        .and_then(|c| c.as_str())
        .and_then(Credential::from_hex)
        .ok_or_else(|| ErroreAvvio::RigaNonValida("credenziale mancante o malformata".into()))?;
    Ok(Pronto {
        pid,
        endpoint,
        credenziale,
    })
}

/// Supervisor avviato dalla UI. Alla distruzione chiede l'arresto controllato
/// e, scaduta [`ATTESA_ARRESTO`], termina il processo.
#[derive(Debug)]
pub struct Servizio {
    figlio: Child,
    stdin: Option<ChildStdin>,
    pid: u32,
    comandi: mpsc::UnboundedSender<Comando>,
    stato: Arc<Mutex<StatoServizio>>,
}

fn lock(m: &Mutex<StatoServizio>) -> MutexGuard<'_, StatoServizio> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

impl Servizio {
    /// Avvia il servizio, attende la riga `ready` e apre le connessioni IPC in
    /// thread propri. `ridisegna` è chiamata a ogni cambio di stato.
    pub fn avvia(
        cfg: &ConfigServizio,
        ridisegna: Arc<dyn Fn() + Send + Sync>,
    ) -> Result<Self, ErroreAvvio> {
        let mut figlio = cfg.comando().spawn().map_err(ErroreAvvio::Spawn)?;
        let stdin = figlio.stdin.take();
        let stdout = figlio.stdout.take().ok_or(ErroreAvvio::Timeout)?;

        // La riga `ready` è letta in un thread: `read_line` non ha timeout.
        let (tx_riga, rx_riga) = std::sync::mpsc::channel();
        std::thread::Builder::new()
            .name("servizio-stdout".into())
            .spawn(move || {
                let mut r = BufReader::new(stdout);
                let mut riga = String::new();
                let esito = r.read_line(&mut riga).map(|n| (n, riga));
                let _ = tx_riga.send(esito);
                // Il resto di stdout non è usato: si scarta fino alla chiusura.
                let _ = std::io::copy(&mut r, &mut std::io::sink());
            })
            .map_err(ErroreAvvio::Spawn)?;
        let pronto = match rx_riga.recv_timeout(ATTESA_PRONTO) {
            Ok(Ok((n, riga))) if n > 0 => leggi_pronto(riga.trim()),
            Ok(_) => Err(ErroreAvvio::UscitoPrimaDelPronto(
                figlio.wait().ok().and_then(|s| s.code()),
            )),
            Err(_) => Err(ErroreAvvio::Timeout),
        };
        let pronto = match pronto {
            Ok(p) => p,
            Err(e) => {
                let _ = figlio.kill();
                let _ = figlio.wait();
                return Err(e);
            }
        };

        let stato = Arc::new(Mutex::new(StatoServizio {
            connessione: CONNESSIONE_IN_CORSO.into(),
            ..StatoServizio::default()
        }));
        let (comandi, rx) = mpsc::unbounded_channel();
        let Pronto {
            pid,
            endpoint,
            credenziale,
        } = pronto;
        let s2 = stato.clone();
        let tx2 = comandi.clone();
        std::thread::Builder::new()
            .name("servizio-ipc".into())
            .spawn(move || {
                let rt = match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(rt) => rt,
                    Err(e) => {
                        lock(&s2).connessione = format!("runtime IPC non avviato: {e}");
                        ridisegna();
                        return;
                    }
                };
                rt.block_on(ciclo_ipc(endpoint, credenziale, rx, tx2, s2, ridisegna));
            })
            .map_err(ErroreAvvio::Spawn)?;
        Ok(Self {
            figlio,
            stdin,
            pid,
            comandi,
            stato,
        })
    }

    /// PID del supervisor riportato nella riga `ready`.
    pub fn pid(&self) -> u32 {
        self.pid
    }

    /// Copia dello stato corrente.
    pub fn stato(&self) -> StatoServizio {
        lock(&self.stato).clone()
    }

    /// Accoda un'azione. Restituisce `false` se il canale IPC è chiuso.
    pub fn invia(&self, c: Comando) -> bool {
        self.comandi.send(c).is_ok()
    }

    /// Il processo del servizio è ancora vivo.
    pub fn vivo(&mut self) -> bool {
        matches!(self.figlio.try_wait(), Ok(None))
    }

    /// Arresto controllato: riga `shutdown` su stdin, poi chiusura della pipe
    /// di vita; scaduta `attesa`, terminazione forzata. Restituisce il codice
    /// di uscita e se è stato necessario forzare.
    pub fn arresta(&mut self, attesa: Duration) -> (Option<i32>, bool) {
        if let Some(mut s) = self.stdin.take() {
            let _ = s.write_all(b"shutdown\n").and_then(|()| s.flush());
        }
        let t0 = Instant::now();
        loop {
            match self.figlio.try_wait() {
                Ok(Some(st)) => return (st.code(), false),
                Ok(None) if t0.elapsed() < attesa => {
                    std::thread::sleep(Duration::from_millis(50));
                }
                _ => break,
            }
        }
        let _ = self.figlio.kill();
        let code = self.figlio.wait().ok().and_then(|s| s.code());
        (code, true)
    }
}

impl Drop for Servizio {
    fn drop(&mut self) {
        if self.vivo() {
            let _ = self.arresta(ATTESA_ARRESTO);
        } else {
            let _ = self.figlio.wait();
        }
    }
}

/// Etichetta leggibile dello stato di un'operazione.
pub fn stato_operazione(s: OperationStatus) -> &'static str {
    match s {
        OperationStatus::Queued => "in coda",
        OperationStatus::Forwarded => "inoltrato",
        OperationStatus::Succeeded => "riuscito",
        OperationStatus::Failed => "fallito",
        OperationStatus::Discarded => "scartato",
    }
}

fn descrivi_errore(m: &ServerMessage) -> String {
    match m {
        ServerMessage::Error { error, .. } => match error {
            ReplyError::Operational(e) => format!("{}: {}", e.code.as_str(), e.message),
            ReplyError::Protocol(e) => format!("protocollo: {e}"),
        },
        ServerMessage::Closing { reason } => format!("connessione chiusa dal servizio: {reason:?}"),
        other => format!("risposta inattesa: {other:?}"),
    }
}

async fn connetti(endpoint: &str, cred: &Credential) -> Result<Client, String> {
    let mut c = Client::connect(endpoint)
        .await
        .map_err(|e| format!("connessione IPC fallita: {e}"))?;
    match c.hello(cred).await {
        Ok(ServerMessage::HelloOk { .. }) => Ok(c),
        Ok(m) => Err(descrivi_errore(&m)),
        Err(e) => Err(format!("handshake fallito: {e}")),
    }
}

async fn ciclo_ipc(
    endpoint: String,
    cred: Credential,
    mut rx: mpsc::UnboundedReceiver<Comando>,
    tx: mpsc::UnboundedSender<Comando>,
    stato: Arc<Mutex<StatoServizio>>,
    ridisegna: Arc<dyn Fn() + Send + Sync>,
) {
    let aggiorna = |f: &dyn Fn(&mut StatoServizio)| {
        f(&mut lock(&stato));
        ridisegna();
    };
    let connessioni = async {
        let r = connetti(&endpoint, &cred).await?;
        let e = connetti(&endpoint, &cred).await?;
        Ok::<_, String>((r, e))
    }
    .await;
    drop(cred);
    let (mut richieste, mut eventi) = match connessioni {
        Ok(x) => x,
        Err(m) => {
            aggiorna(&|s| s.connessione = m.clone());
            return;
        }
    };
    match eventi.request(Request::Subscribe).await {
        Ok(ServerMessage::Response {
            result: ResponseBody::Subscribed,
            ..
        }) => {}
        Ok(m) => {
            let d = descrivi_errore(&m);
            aggiorna(&|s| s.connessione = format!("iscrizione agli eventi rifiutata: {d}"));
            return;
        }
        Err(e) => {
            aggiorna(&|s| s.connessione = format!("iscrizione agli eventi fallita: {e}"));
            return;
        }
    }
    aggiorna(&|s| {
        s.connessione = "connesso".into();
        s.connesso = true;
    });
    let _ = tx.send(Comando::Aggiorna);

    let stato_ev = stato.clone();
    let ridisegna_ev = ridisegna.clone();
    let lettore_eventi = async move {
        loop {
            match eventi.next_event().await {
                Ok(Ok(ServiceEvent::VmState { vm_id, state })) => {
                    {
                        let mut s = lock(&stato_ev);
                        if let Some(v) = s.vms.iter_mut().find(|v| v.vm_id == vm_id) {
                            v.state = state;
                        }
                    }
                    ridisegna_ev();
                    // Il run_id cambia con l'avvio: si rilegge l'elenco.
                    let _ = tx.send(Comando::Aggiorna);
                }
                Ok(Ok(_)) => {}
                Ok(Err(m)) => {
                    let d = descrivi_errore(&m);
                    let mut s = lock(&stato_ev);
                    s.connessione = d;
                    s.connesso = false;
                    drop(s);
                    ridisegna_ev();
                    return;
                }
                Err(e) => {
                    let mut s = lock(&stato_ev);
                    s.connessione = format!("canale eventi chiuso: {e}");
                    s.connesso = false;
                    drop(s);
                    ridisegna_ev();
                    return;
                }
            }
        }
    };

    let esecutore = async {
        while let Some(c) = rx.recv().await {
            let esito = esegui(&mut richieste, &stato, c).await;
            if let Err(m) = esito {
                aggiorna(&|s| s.ultimo_esito = Some(m.clone()));
            } else {
                ridisegna();
            }
        }
    };
    tokio::join!(lettore_eventi, esecutore);
}

async fn esegui(
    c: &mut Client,
    stato: &Mutex<StatoServizio>,
    comando: Comando,
) -> Result<(), String> {
    match comando {
        Comando::Aggiorna => {
            let caps = c
                .request(Request::GetCapabilities)
                .await
                .map_err(|e| e.to_string())?;
            if let ServerMessage::Response {
                result: ResponseBody::Capabilities(k),
                ..
            } = caps
            {
                let mut s = lock(stato);
                s.backend = Some(k.backend);
                s.acceleratore = k.accelerator;
                s.politica = Some(k.parent_loss_policy);
            }
            match c
                .request(Request::ListVms)
                .await
                .map_err(|e| e.to_string())?
            {
                ServerMessage::Response {
                    result: ResponseBody::VmList { vms },
                    ..
                } => {
                    lock(stato).vms = vms;
                    Ok(())
                }
                m => Err(descrivi_errore(&m)),
            }
        }
        Comando::Crea(config) => {
            match c
                .request(Request::CreateVm { config: *config })
                .await
                .map_err(|e| e.to_string())?
            {
                ServerMessage::Response {
                    result: ResponseBody::VmCreated { vm },
                    ..
                } => {
                    lock(stato).ultimo_esito = Some(format!("creata: {}", vm.config.name.as_str()));
                    Box::pin(esegui(c, stato, Comando::Aggiorna)).await
                }
                m => Err(format!("creazione: {}", descrivi_errore(&m))),
            }
        }
        Comando::ImpostaPolitica(policy) => {
            match c
                .request(Request::SetParentLossPolicy { policy })
                .await
                .map_err(|e| e.to_string())?
            {
                ServerMessage::Response {
                    result: ResponseBody::ParentLossPolicy { policy },
                    ..
                } => {
                    let mut s = lock(stato);
                    s.politica = Some(policy);
                    s.ultimo_esito = Some(format!(
                        "alla chiusura anomala della UI: {}",
                        crate::impostazioni::descrivi(policy)
                    ));
                    Ok(())
                }
                m => Err(format!("impostazione: {}", descrivi_errore(&m))),
            }
        }
        Comando::ApriImpostazioni(_) => Ok(()),
        Comando::AggiornaVm {
            vm_id,
            revisione,
            config,
        } => {
            aggiorna_vm(c, stato, vm_id, revisione, *config).await?;
            Box::pin(esegui(c, stato, Comando::Aggiorna)).await
        }
        Comando::ApriVm {
            nome,
            modifiche,
            crea,
        } => {
            Box::pin(esegui(c, stato, Comando::Aggiorna)).await?;
            let trovata = lock(stato)
                .vms
                .iter()
                .find(|v| v.name.as_str() == nome)
                .cloned();
            let vm = match trovata {
                Some(v) => v,
                None if crea => {
                    let mut config = crate::impostazioni_vm::config_predefinita(&nome)?;
                    modifiche.applica(&mut config);
                    Box::pin(esegui(c, stato, Comando::Crea(Box::new(config)))).await?;
                    lock(stato)
                        .vms
                        .iter()
                        .find(|v| v.name.as_str() == nome)
                        .cloned()
                        .ok_or_else(|| format!("VM \"{nome}\" non trovata dopo la creazione"))?
                }
                None => return Err(format!("VM \"{nome}\" non trovata nel catalogo")),
            };
            let spenta = vm.state == lightvm_proto::VmState::Stopped;
            if !modifiche.vuote() {
                if !spenta {
                    return Err(format!(
                        "VM \"{nome}\" accesa: risorse e limiti si cambiano solo a VM spenta"
                    ));
                }
                let mut config = vm.config.clone();
                modifiche.applica(&mut config);
                if config != vm.config {
                    aggiorna_vm(c, stato, vm.vm_id, vm.config_revision, config).await?;
                }
            }
            let avvio = if spenta {
                azione(c, stato, vm.vm_id, VmAction::Start, "avvio").await
            } else {
                Ok(())
            };
            // L'elenco si rilegge anche se l'avvio fallisce: le modifiche
            // salvate restano visibili.
            Box::pin(esegui(c, stato, Comando::Aggiorna)).await?;
            avvio
        }
        Comando::Avvia(vm_id) => azione(c, stato, vm_id, VmAction::Start, "avvio").await,
        Comando::Arresta(vm_id) => azione(c, stato, vm_id, VmAction::Stop, "arresto").await,
        Comando::EmergencyStop => {
            match c
                .request(Request::EmergencyStop)
                .await
                .map_err(|e| e.to_string())?
            {
                ServerMessage::Response {
                    result: ResponseBody::EmergencyStop(r),
                    ..
                } => {
                    lock(stato).ultimo_esito = Some(format!(
                        "Emergency Stop: {} processi terminati, completato: {}",
                        r.processes_terminated,
                        if r.completed { "sì" } else { "no" }
                    ));
                    Ok(())
                }
                m => Err(descrivi_errore(&m)),
            }
        }
    }
}

async fn aggiorna_vm(
    c: &mut Client,
    stato: &Mutex<StatoServizio>,
    vm_id: VmId,
    revisione: u64,
    config: VmConfig,
) -> Result<(), String> {
    match c
        .request(Request::UpdateVm {
            vm_id,
            expected_revision: revisione,
            config,
        })
        .await
        .map_err(|e| e.to_string())?
    {
        ServerMessage::Response {
            result: ResponseBody::VmUpdated { vm },
            ..
        } => {
            lock(stato).ultimo_esito =
                Some(format!("impostazioni salvate: {}", vm.config.name.as_str()));
            Ok(())
        }
        m => Err(format!("impostazioni: {}", descrivi_errore(&m))),
    }
}

async fn azione(
    c: &mut Client,
    stato: &Mutex<StatoServizio>,
    vm_id: VmId,
    action: VmAction,
    nome: &str,
) -> Result<(), String> {
    let req = VmRequest {
        operation_id: OperationId::new_v7(),
        vm_id,
        expected_run_id: None,
        action,
    };
    match c
        .request(Request::SubmitVm { request: req })
        .await
        .map_err(|e| e.to_string())?
    {
        ServerMessage::Response {
            result: ResponseBody::Operation { operation, .. },
            ..
        } => {
            let esito = operation.map_or_else(
                || format!("{nome}: accettato"),
                |o| match o.error {
                    Some(code) => {
                        format!("{nome}: {} ({})", stato_operazione(o.status), code.as_str())
                    }
                    None => format!("{nome}: {}", stato_operazione(o.status)),
                },
            );
            lock(stato).ultimo_esito = Some(esito);
            Ok(())
        }
        m => Err(format!("{nome}: {}", descrivi_errore(&m))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn riga_ready_valida() {
        let cred = Credential::generate().unwrap();
        let riga = format!(
            r#"{{"event":"ready","pid":42,"endpoint":"\\\\.\\pipe\\x","protocol_versions":[1],"credential":"{}"}}"#,
            cred.to_hex()
        );
        let p = leggi_pronto(&riga).unwrap();
        assert_eq!(p.pid, 42);
        assert_eq!(p.endpoint, r"\\.\pipe\x");
        assert!(p.credenziale.ct_eq(&cred));
    }

    #[test]
    fn riga_ready_non_valida() {
        for riga in [
            "",
            "non json",
            r#"{"event":"altro","pid":1,"endpoint":"e","credential":"00"}"#,
            r#"{"event":"ready","endpoint":"e","credential":"00"}"#,
            r#"{"event":"ready","pid":1,"credential":"00"}"#,
            r#"{"event":"ready","pid":1,"endpoint":"e","credential":"zz"}"#,
        ] {
            assert!(
                matches!(leggi_pronto(riga), Err(ErroreAvvio::RigaNonValida(_))),
                "{riga}"
            );
        }
    }
}

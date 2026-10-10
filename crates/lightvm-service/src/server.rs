//! Servizio: endpoint IPC, handshake, registro delle connessioni con revoca,
//! dispatcher delle richieste, arresto controllato.

use std::collections::HashMap;
use std::io;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, mpsc};
use std::time::Duration;

use tokio::io::{AsyncWrite, ReadHalf, WriteHalf};
use tokio::sync::{Semaphore, broadcast, oneshot, watch};

use lightvm_proto::ipc::{
    ClientMessage, CloseReason, ConnectionInfo, EmergencyStopReport, Request, RequestId,
    ResponseBody, Role, SUPPORTED_PROTOCOL_VERSIONS, ServerMessage, ServiceEvent, ServiceState,
    StopReason,
};
use lightvm_proto::{Caller, ErrorCode, ProtocolError, ProtocolErrorCode, PublicError, ReplyError};

use crate::credential::{Credential, CredentialId, CredentialStore};
use crate::engine::VmEngine;
use crate::frame::{self, FrameError};
use crate::lock::{InstanceLock, LockError};
use crate::ratelimit::{RateLimit, TokenBucket};
use crate::supervisor::{
    Backend, Command, EmergencyStop, ParentLossPolicy, SupervisorRequest, SupervisorThread,
    unauthenticated,
};
use crate::transport::{Accepted, BoxedStream, EndpointOptions, Listener, PeerInfo};

/// Configurazione del servizio.
#[derive(Debug, Clone)]
pub struct ServiceConfig {
    /// Directory del catalogo (contiene il lock di istanza singola).
    pub catalog_dir: PathBuf,
    /// Directory privata per il socket (solo Unix).
    pub runtime_dir: Option<PathBuf>,
    /// Limite di byte per frame.
    pub max_frame: u32,
    /// Rate limit per connessione.
    pub rate_limit: RateLimit,
    /// Posti nella coda verso il supervisor (condivisa tra le connessioni).
    pub queue_capacity: usize,
    /// Connessioni simultanee massime (incluse quelle in handshake).
    pub max_connections: usize,
    /// Tempo massimo per completare l'handshake.
    pub handshake_timeout: Duration,
    /// Durata delle credenziali di pairing.
    pub pairing_ttl: Duration,
    /// Attesa massima del supervisor durante l'arresto.
    pub shutdown_timeout: Duration,
    /// Sorte delle VM alla perdita della UI (modificabile via IPC).
    pub parent_loss_policy: ParentLossPolicy,
}

impl ServiceConfig {
    /// Valori predefiniti per un catalogo.
    pub fn new(catalog_dir: impl Into<PathBuf>) -> Self {
        Self {
            catalog_dir: catalog_dir.into(),
            runtime_dir: None,
            max_frame: frame::DEFAULT_MAX_FRAME,
            rate_limit: RateLimit::default(),
            queue_capacity: 64,
            max_connections: 16,
            handshake_timeout: Duration::from_secs(5),
            pairing_ttl: Duration::from_secs(600),
            shutdown_timeout: Duration::from_secs(30),
            parent_loss_policy: ParentLossPolicy::default(),
        }
    }
}

/// Errore di avvio.
#[derive(Debug)]
pub enum StartError {
    /// Lock del catalogo non acquisito.
    Lock(LockError),
    /// Endpoint IPC non creato.
    Endpoint(io::Error),
    /// Altro errore di I/O (generazione credenziale, thread).
    Io(io::Error),
}

impl std::fmt::Display for StartError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Lock(e) => write!(f, "{e}"),
            Self::Endpoint(e) => write!(f, "impossibile creare l'endpoint IPC: {e}"),
            Self::Io(e) => write!(f, "errore di avvio: {e}"),
        }
    }
}

impl std::error::Error for StartError {}

struct ConnEntry {
    role: Role,
    credential_id: CredentialId,
    peer_pid: Option<u32>,
    client_name: Option<String>,
    revoke: watch::Sender<bool>,
}

struct Shared {
    cfg: ServiceConfig,
    creds: Mutex<CredentialStore>,
    conns: Mutex<HashMap<u64, ConnEntry>>,
    next_conn: AtomicU64,
    cmd_tx: Mutex<Option<mpsc::Sender<Command>>>,
    queue: Arc<Semaphore>,
    conn_slots: Arc<Semaphore>,
    events: broadcast::Sender<ServiceEvent>,
    emergency: Arc<dyn EmergencyStop>,
    shutdown_trigger: watch::Sender<bool>,
    closing: watch::Sender<bool>,
    stopping: AtomicBool,
    /// Generazione dell'Emergency Stop: le mutazioni accodate prima di un
    /// Emergency Stop vengono scartate dal supervisor.
    emergency_epoch: Arc<AtomicU64>,
    /// Politica alla perdita della UI in vigore (condivisa col supervisor).
    parent_loss_policy: Arc<Mutex<ParentLossPolicy>>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Errore operativo pubblico con codice stabile.
fn op_error(code: ErrorCode, message: impl Into<String>) -> ReplyError {
    ReplyError::Operational(PublicError::new(code, message))
}

/// Errore di protocollo: nessun effetto tentato.
fn protocol_error(code: ProtocolErrorCode, message: impl Into<String>) -> ReplyError {
    ReplyError::Protocol(ProtocolError {
        code,
        message: message.into(),
    })
}

impl Shared {
    fn send_cmd(&self, cmd: Command) -> bool {
        match lock(&self.cmd_tx).as_ref() {
            Some(tx) => tx.send(cmd).is_ok(),
            None => false,
        }
    }

    /// Revoca una connessione; per i ruoli diversi da `Ui` revoca anche la
    /// credenziale (la sessione non è riutilizzabile).
    fn revoke_connection(&self, id: u64) -> bool {
        let entry = lock(&self.conns).remove(&id);
        match entry {
            Some(e) => {
                if e.role != Role::Ui {
                    lock(&self.creds).revoke(e.credential_id);
                }
                let _ = e.revoke.send(true);
                let _ = self
                    .events
                    .send(ServiceEvent::ConnectionRevoked { connection_id: id });
                true
            }
            None => false,
        }
    }

    /// Revoca tutte le connessioni e credenziali non UI (sessioni agente e pairing).
    fn revoke_non_ui(&self) -> u32 {
        lock(&self.creds).revoke_all_except(Role::Ui);
        let ids: Vec<u64> = lock(&self.conns)
            .iter()
            .filter(|(_, e)| e.role != Role::Ui)
            .map(|(id, _)| *id)
            .collect();
        let mut n = 0;
        for id in ids {
            if self.revoke_connection(id) {
                n += 1;
            }
        }
        n
    }

    fn connections(&self) -> Vec<ConnectionInfo> {
        let mut v: Vec<ConnectionInfo> = lock(&self.conns)
            .iter()
            .map(|(id, e)| ConnectionInfo {
                connection_id: *id,
                role: e.role,
                peer_pid: e.peer_pid,
                client_name: e.client_name.clone(),
            })
            .collect();
        v.sort_by_key(|c| c.connection_id);
        v
    }

    fn trigger_shutdown(&self) {
        // Subito, non solo quando il conduttore dell'arresto parte: un EOF
        // della pipe di vita che segue la richiesta di arresto della UI non è
        // una perdita del genitore (niente Emergency Stop).
        self.stopping.store(true, Ordering::SeqCst);
        let _ = self.shutdown_trigger.send(true);
    }

    async fn emergency_stop(&self) -> EmergencyStopReport {
        // Revoca prima: dopo questo punto nessuna nuova mutazione dell'agente
        // viene accettata. Poi terminazione tramite il gestore del backend,
        // fuori dalla coda e fuori dal thread del supervisor.
        let revoked = self.revoke_non_ui();
        // Le mutazioni già accodate verso il supervisor non vengono eseguite.
        self.emergency_epoch.fetch_add(1, Ordering::SeqCst);
        let handle = self.emergency.clone();
        let outcome = tokio::task::spawn_blocking(move || handle.emergency_stop_all())
            .await
            .unwrap_or_default();
        // Registrazione nel core (audit, stato delle VM) senza attenderla: il
        // supervisor può essere occupato e il percorso prioritario non aspetta.
        self.send_cmd(Command::EmergencyReconcile);
        let _ = self.events.send(ServiceEvent::AgentSessionsStopped {
            reason: StopReason::EmergencyStop,
        });
        EmergencyStopReport {
            revoked_connections: revoked,
            processes_terminated: outcome.processes_terminated,
            completed: outcome.completed,
        }
    }

    async fn parent_lost(self: &Arc<Self>) {
        if self.stopping.load(Ordering::SeqCst) {
            return;
        }
        let policy = *lock(&self.parent_loss_policy);
        self.revoke_non_ui();
        let (done_tx, done_rx) = oneshot::channel();
        if self.send_cmd(Command::ParentLost {
            policy,
            done: done_tx,
        }) {
            let _ = tokio::time::timeout(self.cfg.shutdown_timeout, done_rx).await;
        }
        match policy {
            ParentLossPolicy::KillVms => {
                // Percorso prioritario: le VM terminano subito con primitive OS
                // e il core registra l'Emergency Stop; poi arresto del servizio.
                let r = self.emergency_stop().await;
                crate::diag!(
                    "lightvm-service: UI persa, Emergency Stop: {} processi terminati, completato {}",
                    r.processes_terminated,
                    r.completed
                );
                self.trigger_shutdown();
            }
            ParentLossPolicy::ShutdownVms { .. } => self.trigger_shutdown(),
            ParentLossPolicy::KeepVms => {}
        }
    }

    /// Attesa del supervisor durante l'arresto: con `shutdown_vms` copre
    /// almeno l'attesa ACPI più `quit` e terminazione (20 s).
    fn shutdown_budget(&self) -> Duration {
        match *lock(&self.parent_loss_policy) {
            ParentLossPolicy::ShutdownVms { powerdown_s } => self
                .cfg
                .shutdown_timeout
                .max(Duration::from_secs(u64::from(powerdown_s) + 20)),
            _ => self.cfg.shutdown_timeout,
        }
    }
}

/// Handle del servizio in esecuzione.
pub struct ServiceHandle {
    shared: Arc<Shared>,
    endpoint: String,
    stopped: watch::Receiver<bool>,
}

impl std::fmt::Debug for ServiceHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ServiceHandle")
            .field("endpoint", &self.endpoint)
            .finish_non_exhaustive()
    }
}

impl ServiceHandle {
    /// Endpoint IPC (non segreto).
    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }

    /// Emette una credenziale di pairing (nessun permesso VM).
    pub fn issue_pairing_credential(&self) -> io::Result<Credential> {
        let ttl = self.shared.cfg.pairing_ttl;
        lock(&self.shared.creds)
            .issue(Role::Pairing, Some(ttl))
            .map(|(_, c)| c)
    }

    /// Connessioni attive.
    pub fn connections(&self) -> Vec<ConnectionInfo> {
        self.shared.connections()
    }

    /// Revoca una connessione.
    pub fn revoke_connection(&self, id: u64) -> bool {
        self.shared.revoke_connection(id)
    }

    /// Processo genitore perso: arresto delle sessioni agente, hook del backend
    /// e poi [`ParentLossPolicy`].
    pub async fn parent_lost(&self) {
        self.shared.parent_lost().await;
    }

    /// Emergency Stop dal processo locale (percorso prioritario).
    pub async fn emergency_stop(&self) -> EmergencyStopReport {
        self.shared.emergency_stop().await
    }

    /// Avvia l'arresto controllato senza attenderne la fine.
    pub fn trigger_shutdown(&self) {
        self.shared.trigger_shutdown();
    }

    /// Attende la fine del servizio (per richiesta, genitore perso o shutdown).
    pub async fn wait(&self) {
        let mut rx = self.stopped.clone();
        flag_set(&mut rx).await;
    }

    /// Arresto controllato: chiude le connessioni, attende l'operazione in
    /// corso, rifiuta la coda, arresta il backend e rilascia il lock.
    pub async fn shutdown(self) {
        self.shared.trigger_shutdown();
        self.wait().await;
    }
}

/// Avvia il servizio. Restituisce l'handle e la credenziale iniziale della UI,
/// da consegnare al genitore su un canale privato (mai negli argomenti).
pub async fn start(
    cfg: ServiceConfig,
    engine: Box<dyn VmEngine>,
    backend: Box<dyn Backend>,
) -> Result<(ServiceHandle, Credential), StartError> {
    let instance_lock = InstanceLock::acquire(&cfg.catalog_dir).map_err(StartError::Lock)?;
    let listener = Listener::bind(&EndpointOptions {
        runtime_dir: cfg.runtime_dir.clone(),
        catalog_dir: cfg.catalog_dir.clone(),
    })
    .map_err(StartError::Endpoint)?;
    let endpoint = listener.endpoint().to_owned();

    let mut creds = CredentialStore::new();
    let (_, ui_credential) = creds.issue(Role::Ui, None).map_err(StartError::Io)?;

    let (events, _) = broadcast::channel(256);
    let (cmd_tx, cmd_rx) = mpsc::channel();
    let emergency = backend.emergency_handle();
    let emergency_epoch = Arc::new(AtomicU64::new(0));
    let parent_loss_policy = Arc::new(Mutex::new(cfg.parent_loss_policy));
    let sup = SupervisorThread {
        engine,
        backend,
        events: events.clone(),
        max_frame: cfg.max_frame,
        emergency_epoch: emergency_epoch.clone(),
        parent_loss_policy: parent_loss_policy.clone(),
    };
    // Pubblicato prima di qualunque iscrizione possibile: nessun client lo
    // riceve in ritardo, dopo eventi successivi (corsa con `subscribe`).
    let _ = events.send(ServiceEvent::ServiceState {
        state: ServiceState::Running,
    });
    let sup_thread = std::thread::Builder::new()
        .name("lightvm-supervisor".into())
        .spawn(move || sup.run(cmd_rx))
        .map_err(StartError::Io)?;

    let (shutdown_trigger, mut shutdown_rx) = watch::channel(false);
    let (closing, _) = watch::channel(false);
    let (stopped_tx, stopped_rx) = watch::channel(false);

    let shared = Arc::new(Shared {
        queue: Arc::new(Semaphore::new(cfg.queue_capacity)),
        conn_slots: Arc::new(Semaphore::new(cfg.max_connections)),
        cfg,
        creds: Mutex::new(creds),
        conns: Mutex::new(HashMap::new()),
        next_conn: AtomicU64::new(1),
        cmd_tx: Mutex::new(Some(cmd_tx)),
        events,
        emergency,
        shutdown_trigger,
        closing,
        stopping: AtomicBool::new(false),
        emergency_epoch,
        parent_loss_policy,
    });

    // Ciclo di accettazione.
    let accept_shared = shared.clone();
    let mut accept_closing = shared.closing.subscribe();
    let accept_task = tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = flag_set(&mut accept_closing) => break,
                r = listener.accept() => match r {
                    Ok(Accepted::Ok(stream, peer)) => {
                        let sh = accept_shared.clone();
                        match sh.conn_slots.clone().try_acquire_owned() {
                            Ok(permit) => {
                                tokio::spawn(async move {
                                    handle_connection(sh, stream, peer).await;
                                    drop(permit);
                                });
                            }
                            Err(_) => {
                                tokio::spawn(reject_busy(stream, accept_shared.cfg.max_frame));
                            }
                        }
                    }
                    Ok(Accepted::Rejected(reason)) => {
                        crate::diag!("lightvm-service: peer rifiutato dal trasporto: {reason}");
                    }
                    Err(e) => {
                        crate::diag!("lightvm-service: errore di accettazione: {e}");
                        tokio::time::sleep(Duration::from_millis(50)).await;
                    }
                },
            }
        }
        // `listener` viene rilasciato qui: su Unix il socket viene rimosso.
    });

    // Conduttore dell'arresto.
    let drv = shared.clone();
    tokio::spawn(async move {
        flag_set(&mut shutdown_rx).await;
        drv.stopping.store(true, Ordering::SeqCst);
        // 1. Nessuna nuova connessione; le connessioni ricevono `closing`.
        let _ = drv.closing.send(true);
        let _ = accept_task.await;
        // 2. Il supervisor termina l'operazione corrente, rifiuta la coda e
        //    arresta il backend.
        let (done_tx, done_rx) = oneshot::channel();
        let finished = drv.send_cmd(Command::Shutdown { done: done_tx })
            && tokio::time::timeout(drv.shutdown_budget(), done_rx)
                .await
                .is_ok();
        lock(&drv.cmd_tx).take();
        if finished {
            let _ = tokio::task::spawn_blocking(move || sup_thread.join()).await;
        } else {
            // Supervisor bloccato: non si attende il thread; le VM ancora vive
            // terminano con il percorso prioritario (non con la sola chiusura
            // del Job Object all'uscita), poi il lock viene rilasciato.
            crate::diag!("lightvm-service: timeout nell'arresto del supervisor");
            let handle = drv.emergency.clone();
            let outcome = tokio::task::spawn_blocking(move || handle.emergency_stop_all())
                .await
                .unwrap_or_default();
            crate::diag!(
                "lightvm-service: Emergency Stop dopo il timeout: {} processi terminati",
                outcome.processes_terminated
            );
        }
        // 3. Attesa breve della chiusura delle connessioni.
        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        while drv.conn_slots.available_permits() < drv.cfg.max_connections
            && tokio::time::Instant::now() < deadline
        {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        // 4. Rilascio del lock del catalogo.
        drop(instance_lock);
        let _ = stopped_tx.send(true);
    });

    Ok((
        ServiceHandle {
            shared,
            endpoint,
            stopped: stopped_rx,
        },
        ui_credential,
    ))
}

async fn reject_busy(stream: BoxedStream, max: u32) {
    let (_, mut wr) = tokio::io::split(stream);
    let _ = send(
        &mut wr,
        &ServerMessage::Error {
            request_id: None,
            error: op_error(ErrorCode::ResourceLimit, "troppe connessioni"),
        },
        max,
    )
    .await;
}

async fn send<W: AsyncWrite + Unpin>(w: &mut W, m: &ServerMessage, max: u32) -> io::Result<()> {
    frame::write_frame(w, m, max).await
}

fn err(request_id: Option<RequestId>, e: ErrorCode, msg: &str) -> ServerMessage {
    ServerMessage::Error {
        request_id,
        error: op_error(e, msg),
    }
}

/// Lettore dedicato: la lettura di un frame non è cancellabile a metà senza
/// corrompere il flusso, quindi avviene in un task separato che consegna frame
/// completi su un canale limitato (contropressione per connessione).
fn spawn_reader(
    mut rd: ReadHalf<BoxedStream>,
    max: u32,
) -> (
    tokio::sync::mpsc::Receiver<Result<ClientMessage, FrameError>>,
    AbortOnDrop,
) {
    let (tx, rx) = tokio::sync::mpsc::channel(4);
    let task = tokio::spawn(async move {
        loop {
            let r = frame::read_frame::<_, ClientMessage>(&mut rd, max).await;
            let fatal = matches!(
                r,
                Err(FrameError::Closed
                    | FrameError::Io(_)
                    | FrameError::TooLarge { .. }
                    | FrameError::Empty)
            );
            if tx.send(r).await.is_err() || fatal {
                break;
            }
        }
    });
    (rx, AbortOnDrop(task))
}

/// Interrompe il lettore quando la connessione termina: la metà di lettura
/// viene rilasciata e il trasporto si chiude davvero.
struct AbortOnDrop(tokio::task::JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

fn frame_error_message(e: &FrameError) -> Option<ServerMessage> {
    match e {
        FrameError::TooLarge { declared, max } => Some(ServerMessage::Error {
            request_id: None,
            error: protocol_error(
                ProtocolErrorCode::MessageTooLarge,
                format!("frame di {declared} byte oltre il limite di {max}"),
            ),
        }),
        FrameError::Empty => Some(ServerMessage::Error {
            request_id: None,
            error: protocol_error(ProtocolErrorCode::MalformedMessage, "frame vuoto"),
        }),
        FrameError::Malformed(_) => Some(ServerMessage::Error {
            request_id: None,
            error: protocol_error(ProtocolErrorCode::MalformedMessage, "messaggio non valido"),
        }),
        FrameError::Closed | FrameError::Io(_) => None,
    }
}

struct Authenticated {
    connection_id: u64,
    role: Role,
    revoke_rx: watch::Receiver<bool>,
}

/// Handshake: versione, poi credenziale. Ogni rifiuto chiude la connessione
/// senza effetti e senza distinguere verso il client i motivi del diniego.
async fn handshake(
    shared: &Arc<Shared>,
    frames: &mut tokio::sync::mpsc::Receiver<Result<ClientMessage, FrameError>>,
    wr: &mut WriteHalf<BoxedStream>,
    peer: &PeerInfo,
) -> Option<Authenticated> {
    let max = shared.cfg.max_frame;
    let first = match tokio::time::timeout(shared.cfg.handshake_timeout, frames.recv()).await {
        Err(_) | Ok(None) => return None,
        Ok(Some(r)) => r,
    };
    let (request_id, versions, credential, client_name) = match first {
        Err(e) => {
            if let Some(m) = frame_error_message(&e) {
                let _ = send(wr, &m, max).await;
            }
            return None;
        }
        Ok(ClientMessage::Request { request_id, .. }) => {
            let _ = send(
                wr,
                &ServerMessage::Error {
                    request_id: Some(request_id),
                    error: unauthenticated("handshake con credenziale richiesto"),
                },
                max,
            )
            .await;
            return None;
        }
        Ok(ClientMessage::Hello {
            request_id,
            protocol_versions,
            credential,
            client_name,
        }) => (request_id, protocol_versions, credential, client_name),
    };
    let Some(version) = SUPPORTED_PROTOCOL_VERSIONS
        .iter()
        .copied()
        .find(|v| versions.contains(v))
    else {
        let _ = send(
            wr,
            &ServerMessage::Error {
                request_id: Some(request_id),
                error: protocol_error(
                    ProtocolErrorCode::UnsupportedVersion,
                    format!(
                        "nessuna versione di protocollo comune; supportate: {SUPPORTED_PROTOCOL_VERSIONS:?}"
                    ),
                ),
            },
            max,
        )
        .await;
        return None;
    };
    let verified = credential
        .as_deref()
        .and_then(Credential::from_hex)
        .and_then(|c| lock(&shared.creds).verify(&c));
    let Some((credential_id, role)) = verified else {
        let _ = send(
            wr,
            &ServerMessage::Error {
                request_id: Some(request_id),
                error: unauthenticated("credenziale assente o non valida"),
            },
            max,
        )
        .await;
        return None;
    };
    if shared.stopping.load(Ordering::SeqCst) {
        let _ = send(
            wr,
            &err(
                Some(request_id),
                ErrorCode::StateConflict,
                "servizio in arresto",
            ),
            max,
        )
        .await;
        return None;
    }
    let connection_id = shared.next_conn.fetch_add(1, Ordering::SeqCst);
    let (revoke, revoke_rx) = watch::channel(false);
    lock(&shared.conns).insert(
        connection_id,
        ConnEntry {
            role,
            credential_id,
            peer_pid: peer.pid,
            client_name: client_name.map(|n| n.chars().take(64).collect()),
            revoke,
        },
    );
    let ok = ServerMessage::HelloOk {
        request_id,
        protocol_version: version,
        connection_id,
        role,
    };
    if send(wr, &ok, max).await.is_err() {
        lock(&shared.conns).remove(&connection_id);
        return None;
    }
    Some(Authenticated {
        connection_id,
        role,
        revoke_rx,
    })
}

enum Flow {
    Continue,
    Close(Option<CloseReason>),
}

async fn handle_connection(shared: Arc<Shared>, stream: BoxedStream, peer: PeerInfo) {
    let max = shared.cfg.max_frame;
    let (rd, mut wr) = tokio::io::split(stream);
    let (mut frames, _reader) = spawn_reader(rd, max);
    let Some(auth) = handshake(&shared, &mut frames, &mut wr, &peer).await else {
        return;
    };
    let Authenticated {
        connection_id,
        role,
        mut revoke_rx,
    } = auth;
    let mut closing_rx = shared.closing.subscribe();
    let mut bucket = TokenBucket::new(shared.cfg.rate_limit);
    let mut events: Option<broadcast::Receiver<ServiceEvent>> = None;

    let close_reason: Option<CloseReason> = loop {
        let flow = tokio::select! {
            biased;
            _ = flag_set(&mut revoke_rx) => Flow::Close(Some(CloseReason::Revoked)),
            _ = flag_set(&mut closing_rx) => Flow::Close(Some(CloseReason::ServiceStopping)),
            ev = recv_event(&mut events) => {
                let msg = match ev {
                    Ok(event) => ServerMessage::Event { event },
                    Err(broadcast::error::RecvError::Lagged(n)) => ServerMessage::Event {
                        event: ServiceEvent::EventsLost { count: n },
                    },
                    Err(broadcast::error::RecvError::Closed) => {
                        events = None;
                        continue;
                    }
                };
                if send(&mut wr, &msg, max).await.is_err() { Flow::Close(None) } else { Flow::Continue }
            }
            f = frames.recv() => match f {
                None | Some(Err(FrameError::Closed | FrameError::Io(_))) => Flow::Close(None),
                Some(Err(e)) => {
                    let fatal = matches!(e, FrameError::TooLarge { .. } | FrameError::Empty);
                    if let Some(m) = frame_error_message(&e) {
                        let _ = send(&mut wr, &m, max).await;
                    }
                    if fatal { Flow::Close(None) } else { Flow::Continue }
                }
                Some(Ok(ClientMessage::Hello { request_id, .. })) => {
                    let m = ServerMessage::Error {
                        request_id: Some(request_id),
                        error: protocol_error(ProtocolErrorCode::MalformedMessage, "handshake già completato"),
                    };
                    if send(&mut wr, &m, max).await.is_err() { Flow::Close(None) } else { Flow::Continue }
                }
                Some(Ok(ClientMessage::Request { request_id, body })) => {
                    if !bucket.try_acquire() {
                        let m = err(Some(request_id), ErrorCode::ResourceLimit, &format!(
                            "frequenza delle richieste oltre il limite; riprovare tra {} ms",
                            bucket.retry_after().as_millis()
                        ));
                        if send(&mut wr, &m, max).await.is_err() { Flow::Close(None) } else { Flow::Continue }
                    } else {
                        let ctx = ReqCtx { shared: &shared, role, revoke_rx: &mut revoke_rx };
                        match dispatch(ctx, &mut events, body).await {
                            Dispatch::Reply(res) => {
                                let m = match res {
                                    Ok(result) => ServerMessage::Response { request_id, result },
                                    Err(error) => ServerMessage::Error { request_id: Some(request_id), error },
                                };
                                if send(&mut wr, &m, max).await.is_err() { Flow::Close(None) } else { Flow::Continue }
                            }
                            Dispatch::Interrupted(reason) => Flow::Close(Some(reason)),
                            Dispatch::ReplyThenShutdown(result) => {
                                let _ = send(&mut wr, &ServerMessage::Response { request_id, result }, max).await;
                                shared.trigger_shutdown();
                                Flow::Continue
                            }
                        }
                    }
                }
            },
        };
        match flow {
            Flow::Continue => {}
            Flow::Close(reason) => break reason,
        }
    };
    // Eventi già pubblicati (per esempio l'arresto delle sessioni agente)
    // vengono consegnati prima della notifica di chiusura.
    if let Some(rx) = events.as_mut() {
        while let Ok(event) = rx.try_recv() {
            if send(&mut wr, &ServerMessage::Event { event }, max)
                .await
                .is_err()
            {
                break;
            }
        }
    }
    if let Some(reason) = close_reason {
        let _ = send(&mut wr, &ServerMessage::Closing { reason }, max).await;
    }
    lock(&shared.conns).remove(&connection_id);
    let _ = tokio::io::AsyncWriteExt::shutdown(&mut wr).await;
}

async fn recv_event(
    events: &mut Option<broadcast::Receiver<ServiceEvent>>,
) -> Result<ServiceEvent, broadcast::error::RecvError> {
    match events {
        Some(rx) => rx.recv().await,
        None => std::future::pending().await,
    }
}

struct ReqCtx<'a> {
    shared: &'a Arc<Shared>,
    role: Role,
    revoke_rx: &'a mut watch::Receiver<bool>,
}

enum Dispatch {
    Reply(Result<ResponseBody, ReplyError>),
    ReplyThenShutdown(ResponseBody),
    Interrupted(CloseReason),
}

fn requires_ui(req: &Request) -> bool {
    !matches!(req, Request::Ping | Request::GetCapabilities)
}

/// Chiamante verso il core: la UI è l'utente locale; il pairing non ha
/// alcun permesso sulle VM (il grant di sessione agente arriva in F4).
fn caller_for(role: Role) -> Option<Caller> {
    match role {
        Role::Ui => Some(Caller::User),
        Role::Pairing => None,
    }
}

async fn dispatch(
    ctx: ReqCtx<'_>,
    events: &mut Option<broadcast::Receiver<ServiceEvent>>,
    req: Request,
) -> Dispatch {
    let shared = ctx.shared;
    // Primo controllo: al ricevimento.
    if requires_ui(&req) && ctx.role != Role::Ui {
        return Dispatch::Reply(Err(op_error(
            ErrorCode::PermissionDenied,
            format!("operazione {} non consentita al ruolo", req.name()),
        )));
    }
    let sup_req = match req {
        Request::Ping => SupervisorRequest::Ping,
        Request::GetCapabilities => SupervisorRequest::GetCapabilities,
        Request::ListVms => SupervisorRequest::ListVms,
        Request::VmStatus { vm_id } => SupervisorRequest::VmStatus(vm_id),
        Request::CreateVm { config } => SupervisorRequest::CreateVm(Box::new(config)),
        Request::UpdateVm {
            vm_id,
            expected_revision,
            config,
        } => SupervisorRequest::UpdateVm {
            vm_id,
            expected_revision,
            config: Box::new(config),
        },
        Request::SubmitVm { request } => SupervisorRequest::SubmitVm(Box::new(request)),
        Request::Subscribe => {
            if events.is_none() {
                *events = Some(shared.events.subscribe());
            }
            return Dispatch::Reply(Ok(ResponseBody::Subscribed));
        }
        Request::ListConnections => {
            return Dispatch::Reply(Ok(ResponseBody::Connections {
                connections: shared.connections(),
            }));
        }
        Request::RevokeConnection { connection_id } => {
            return Dispatch::Reply(if shared.revoke_connection(connection_id) {
                Ok(ResponseBody::Revoked { connection_id })
            } else {
                Err(op_error(
                    ErrorCode::StateConflict,
                    "connessione inesistente",
                ))
            });
        }
        Request::CreatePairingCredential => {
            let ttl = shared.cfg.pairing_ttl;
            let r = lock(&shared.creds).issue(Role::Pairing, Some(ttl));
            return Dispatch::Reply(match r {
                Ok((_, c)) => Ok(ResponseBody::PairingCredential {
                    credential: c.to_hex(),
                    expires_in_s: ttl.as_secs(),
                }),
                Err(_) => Err(op_error(
                    ErrorCode::ResourceLimit,
                    "generatore casuale non disponibile",
                )),
            });
        }
        Request::EmergencyStop => {
            // Percorso prioritario: nessun posto in coda, nessun passaggio dal
            // thread del supervisor.
            let report = shared.emergency_stop().await;
            return Dispatch::Reply(Ok(ResponseBody::EmergencyStop(report)));
        }
        Request::SetParentLossPolicy { policy } => {
            return Dispatch::Reply(match policy.validate() {
                Ok(policy) => {
                    *lock(&shared.parent_loss_policy) = policy;
                    Ok(ResponseBody::ParentLossPolicy { policy })
                }
                Err(e) => Err(ReplyError::Operational(e)),
            });
        }
        Request::Shutdown => return Dispatch::ReplyThenShutdown(ResponseBody::ShuttingDown),
    };
    let Ok(permit) = shared.queue.clone().try_acquire_owned() else {
        return Dispatch::Reply(Err(op_error(
            ErrorCode::ResourceLimit,
            "coda del supervisor piena",
        )));
    };
    let (reply_tx, reply_rx) = oneshot::channel();
    let sent = shared.send_cmd(Command::Request {
        req: sup_req,
        caller: caller_for(ctx.role),
        revoked: ctx.revoke_rx.clone(),
        epoch: shared.emergency_epoch.load(Ordering::SeqCst),
        reply: reply_tx,
        _permit: permit,
    });
    if !sent {
        return Dispatch::Reply(Err(op_error(
            ErrorCode::StateConflict,
            "servizio in arresto",
        )));
    }
    tokio::select! {
        biased;
        _ = flag_set(ctx.revoke_rx) => Dispatch::Interrupted(CloseReason::Revoked),
        r = reply_rx => Dispatch::Reply(r.unwrap_or_else(|_| Err(op_error(
            ErrorCode::StateConflict,
            "servizio in arresto",
        )))),
    }
}

/// Attende che il flag diventi vero (o che il mittente sparisca) senza
/// trattenere il riferimento al valore oltre l'attesa.
async fn flag_set(rx: &mut watch::Receiver<bool>) {
    let _ = rx.wait_for(|v| *v).await;
}

//! Esecuzione di una VM: processo, QMP, verifiche post-avvio, macchina a stati
//! e arresto (ROADMAP 6.1, 6.3, 7.2, 7.3; ADR-0002, ADR-0007, ADR-0009).
//!
//! Sequenza di [`QemuRun::start`]:
//!
//! 1. validazione della specifica contro le capacità rilevate (acceleratore
//!    verificato dal probe e uguale a quello richiesto, macchina, CPU,
//!    dispositivi);
//! 2. apertura protettiva dei file ([`crate::storage_guard`]);
//! 3. rimozione dei socket orfani, spawn nel contenitore dell'esecuzione (Job
//!    Object), QEMU con `-S`;
//! 4. connessione QMP (unico client, socket nella directory privata);
//! 5. verifica dell'acceleratore attivo (`query-accelerators`) e dell'assenza
//!    di NIC (`info network` vuoto e nessun dispositivo PCI di classe rete);
//!    in caso di esito negativo il processo viene terminato;
//! 6. `cont`: solo ora il guest esegue codice.
//!
//! Stato del processo: `Starting → Running → Stopping → Exited { code, cause }`,
//! osservato dall'attesa sull'handle del processo (thread dedicato) e dagli
//! eventi QMP `SHUTDOWN`. L'[`EmergencyStop`] termina il contenitore con
//! primitive OS senza passare dalla coda QMP. [`QemuRun::reconcile`] confronta
//! processo e QMP; [`QemuRun::close`] chiude il Job Object dopo l'uscita.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use lightvm_proto::{
    BackendObservation, DisplayGeometry, InputEvent, ObservationSource, ObservedState, RunId,
    UtcTimestamp, VmState,
};
use tokio::sync::{broadcast, watch};

use crate::cmdline::{QemuCommand, VmLaunchSpec};
use crate::error::{BackendError, Result};
use crate::input::{PressedInput, validate_events};
use crate::paths::{ManagedPath, ManagedPaths};
use crate::probe::{AcceleratorStatus, HostCapabilities, UnavailableReason, stderr_excerpt};
use crate::process::{
    ChildProcess, ExitCodeClass, ProcessContainer, ProcessIdentity, SpawnRequest,
    classify_exit_code, minimal_environment, new_container,
};
use crate::qmp::{
    AcceleratorsInfo, ConnectionState, CurrentMachine, EventKind, PciNetworkDevice, QmpClient,
    QmpConfig, QmpEvent, RunState, StatusInfo, VersionInfo,
};
use crate::screenshot::{CapturedScreenshot, RgbImage};
use crate::storage_guard::StorageGuards;

pub use crate::process::EMERGENCY_EXIT_CODE;

/// Causa dell'uscita del processo QEMU.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExitCause {
    /// Il guest si è spento (evento `SHUTDOWN` con `guest: true`).
    GuestShutdown,
    /// Uscita dopo `quit` richiesto dal backend.
    QmpQuit,
    /// Terminazione del contenitore richiesta dal backend.
    EmergencyStop,
    /// QEMU è uscito prima di completare l'avvio.
    StartupFailed,
    /// Uscita non richiesta (crash, terminazione esterna).
    Unexpected,
}

/// Stato del processo QEMU di un'esecuzione.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProcessState {
    /// Processo creato, verifiche post-avvio in corso.
    Starting,
    /// Verifiche superate, vCPU avviate (o lasciate in pausa su richiesta).
    Running,
    /// Arresto richiesto.
    Stopping,
    /// Processo terminato (osservato sull'handle).
    Exited {
        /// Codice d'uscita.
        code: Option<u32>,
        /// Causa.
        cause: ExitCause,
    },
}

/// Configurazione di un'esecuzione.
#[derive(Clone, Debug)]
pub struct RunConfig {
    /// Configurazione QMP.
    pub qmp: QmpConfig,
    /// Tempo massimo fra lo spawn e la connessione QMP.
    pub startup_timeout: Duration,
    /// Timeout di `screendump`.
    pub screendump_timeout: Duration,
    /// Dimensione massima del PNG di uno screenshot.
    pub max_screenshot_bytes: u64,
    /// Lascia le vCPU in pausa dopo le verifiche (nessun `cont`).
    pub leave_paused: bool,
}

impl Default for RunConfig {
    fn default() -> Self {
        Self {
            qmp: QmpConfig::default(),
            startup_timeout: Duration::from_secs(20),
            screendump_timeout: Duration::from_secs(10),
            max_screenshot_bytes: 32 * 1024 * 1024,
            leave_paused: false,
        }
    }
}

/// Politica di arresto gentile con scalata (F0-06, 2d): ogni timeout è
/// configurabile.
#[derive(Clone, Copy, Debug)]
pub struct ShutdownPolicy {
    /// Attesa dopo `system_powerdown` (zero = salta il powerdown).
    pub powerdown_timeout: Duration,
    /// Attesa dopo `quit`.
    pub quit_timeout: Duration,
    /// Attesa dopo la terminazione del contenitore.
    pub kill_timeout: Duration,
}

impl Default for ShutdownPolicy {
    fn default() -> Self {
        Self {
            powerdown_timeout: Duration::from_secs(60),
            quit_timeout: Duration::from_secs(5),
            kill_timeout: Duration::from_secs(10),
        }
    }
}

/// Fase con cui si è concluso l'arresto.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StopPhase {
    /// Il processo era già terminato.
    AlreadyExited,
    /// Uscita dopo `system_powerdown`.
    Powerdown,
    /// Uscita dopo `quit`.
    Quit,
    /// Terminazione del contenitore.
    Kill,
}

/// Esito osservato di un arresto.
#[derive(Clone, Debug)]
pub struct ExitReport {
    /// Codice d'uscita.
    pub code: Option<u32>,
    /// Classe del codice d'uscita.
    pub class: ExitCodeClass,
    /// Causa.
    pub cause: ExitCause,
    /// Fase conclusiva.
    pub phase: StopPhase,
    /// Tempo dalla richiesta all'uscita osservata.
    pub elapsed: Duration,
    /// Processi ancora attivi nel contenitore dopo l'uscita (atteso 0).
    pub active_after: u32,
    /// Fasi della scalata non riuscite (comando QMP rifiutato o scaduto,
    /// nessuna uscita entro il timeout), senza testo di QEMU.
    pub notes: Vec<String>,
}

/// Esito di [`QemuRun::close`].
#[derive(Clone, Debug)]
pub struct CloseReport {
    /// Identità del processo dell'esecuzione chiusa.
    pub identity: ProcessIdentity,
    /// `true` se il processo era vivo ed è stato terminato con l'Emergency Stop.
    pub forced: bool,
    /// Processi attivi nel contenitore al momento della chiusura (atteso 0).
    pub active_at_close: u32,
    /// `true` se l'handle del Job Object è stato chiuso qui; `false` se un
    /// [`EmergencyStop`] clonato lo tiene ancora aperto.
    pub job_closed: bool,
    /// `true` se alla chiusura il backend non teneva altri handle del processo
    /// oltre a quello dell'esecuzione, rilasciato qui.
    pub process_handle_released: bool,
    /// File del socket QMP ancora presente dopo la chiusura (atteso `false`).
    pub qmp_socket_left: bool,
}

/// Riconciliazione dello stato dell'esecuzione da processo e QMP.
#[derive(Clone, Debug)]
pub struct Reconciliation {
    /// Stato del processo secondo l'handle (autoritativo sulla vita).
    pub process: ProcessState,
    /// Codice d'uscita letto direttamente dall'handle, se terminato.
    pub handle_exit_code: Option<u32>,
    /// Stato della connessione QMP.
    pub qmp: ConnectionState,
    /// Stato delle vCPU secondo `query-status`, se QMP ha risposto.
    pub qmp_status: Option<RunState>,
    /// Stato pubblico derivato.
    pub observed: ObservedState,
    /// Anomalia rilevata (processo vivo ma QMP perso, guest fermo, ...).
    pub anomaly: Option<String>,
}

#[derive(Debug, Default)]
pub(crate) struct RunFlags {
    quit_requested: AtomicBool,
    stop_requested: AtomicBool,
    emergency: AtomicBool,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Processo avviato con QMP collegato (uso interno: probe ed esecuzioni).
pub(crate) struct Launched {
    pub(crate) container: Arc<dyn ProcessContainer>,
    pub(crate) child: Arc<ChildProcess>,
    pub(crate) qmp: QmpClient,
    pub(crate) state_tx: Arc<watch::Sender<ProcessState>>,
    pub(crate) flags: Arc<RunFlags>,
}

impl Launched {
    /// Attende l'uscita fino a `wait`, poi termina il contenitore.
    pub(crate) async fn finish(self, wait: Duration) {
        let mut rx = self.state_tx.subscribe();
        let exited = tokio::time::timeout(
            wait,
            rx.wait_for(|s| matches!(s, ProcessState::Exited { .. })),
        )
        .await;
        if !matches!(exited, Ok(Ok(_))) {
            self.flags.emergency.store(true, Ordering::SeqCst);
            let _ = self.container.kill_all(EMERGENCY_EXIT_CODE);
            let child = self.child.clone();
            let _ = tokio::task::spawn_blocking(move || child.wait(Some(Duration::from_secs(10))))
                .await;
        }
    }
}

/// Rimuove i socket orfani (QEMU non li cancella dopo un arresto forzato o un
/// errore di avvio, F0-02 §4.3).
fn remove_sockets(cmd: &QemuCommand) {
    let _ = std::fs::remove_file(cmd.qmp_socket.as_path());
    if let Some(v) = &cmd.vnc_socket {
        let _ = std::fs::remove_file(v.as_path());
    }
}

fn read_stderr_tail(p: &ManagedPath) -> String {
    let bytes = std::fs::read(p.as_path()).unwrap_or_default();
    let start = bytes.len().saturating_sub(64 * 1024);
    stderr_excerpt(&String::from_utf8_lossy(&bytes[start..]))
}

/// Avvia QEMU nel contenitore e collega QMP. Il processo è terminato se la
/// connessione non riesce entro `timeout`.
pub(crate) async fn launch(
    cmd: &QemuCommand,
    qmp_cfg: &QmpConfig,
    timeout: Duration,
) -> Result<Launched> {
    if !cmd.private_dir.as_path().is_dir() {
        return Err(BackendError::invalid(
            "private_dir",
            "la directory privata dell'esecuzione non esiste",
        ));
    }
    remove_sockets(cmd);
    let container = new_container()?;
    let req = SpawnRequest {
        run_id: cmd.run_id,
        program: cmd.program.clone(),
        args: cmd.args.clone(),
        env: minimal_environment(),
        stdout: cmd.stdout.clone(),
        stderr: cmd.stderr.clone(),
    };
    let child = {
        let c = container.clone();
        tokio::task::spawn_blocking(move || c.spawn(&req))
            .await
            .map_err(|e| BackendError::Io(std::io::Error::other(e.to_string())))??
    };
    if let Err(e) = container.verify_membership(&child) {
        let _ = container.kill_all(EMERGENCY_EXIT_CODE);
        return Err(e);
    }
    let child = Arc::new(child);
    let state_tx = Arc::new(watch::channel(ProcessState::Starting).0);
    let flags = Arc::new(RunFlags::default());
    let deadline = Instant::now() + timeout;
    let qmp = loop {
        if let Some(code) = child.try_exit_code()? {
            let stderr_tail = read_stderr_tail(&cmd.stderr);
            remove_sockets(cmd);
            state_tx.send_replace(ProcessState::Exited {
                code: Some(code),
                cause: ExitCause::StartupFailed,
            });
            return Err(BackendError::StartupFailed {
                exit_code: Some(code),
                stderr_tail,
            });
        }
        match QmpClient::connect(&cmd.qmp_socket, qmp_cfg.clone()).await {
            Ok(q) => break q,
            Err(e) => {
                if Instant::now() >= deadline {
                    flags.emergency.store(true, Ordering::SeqCst);
                    let _ = container.kill_all(EMERGENCY_EXIT_CODE);
                    let c = child.clone();
                    let _ =
                        tokio::task::spawn_blocking(move || c.wait(Some(Duration::from_secs(10))))
                            .await;
                    remove_sockets(cmd);
                    return Err(BackendError::StartupFailed {
                        exit_code: child.try_exit_code().ok().flatten(),
                        stderr_tail: format!(
                            "QMP non raggiungibile entro {timeout:?} ({e}); {}",
                            read_stderr_tail(&cmd.stderr)
                        ),
                    });
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        }
    };
    spawn_waiter(
        child.clone(),
        flags.clone(),
        state_tx.clone(),
        qmp.subscribe_events(),
        qmp.subscribe_state(),
        cmd.clone(),
    )?;
    Ok(Launched {
        container,
        child,
        qmp,
        state_tx,
        flags,
    })
}

/// Thread che attende l'uscita sull'handle del processo e pubblica lo stato
/// finale con la causa. Prima di classificare attende (al massimo 500 ms) che
/// il lettore QMP abbia consegnato gli ultimi messaggi, così l'evento
/// `SHUTDOWN` del guest non viene perso.
fn spawn_waiter(
    child: Arc<ChildProcess>,
    flags: Arc<RunFlags>,
    state_tx: Arc<watch::Sender<ProcessState>>,
    mut events: broadcast::Receiver<QmpEvent>,
    qmp_state: watch::Receiver<ConnectionState>,
    cmd: QemuCommand,
) -> Result<()> {
    std::thread::Builder::new()
        .name("qemu-waiter".into())
        .spawn(move || {
            let code = child.wait(None).ok().flatten();
            // L'handle serve solo per l'attesa: rilasciarlo subito evita che il
            // processo terminato resti referenziato oltre la chiusura.
            drop(child);
            let t0 = Instant::now();
            while matches!(*qmp_state.borrow(), ConnectionState::Ready)
                && t0.elapsed() < Duration::from_millis(500)
            {
                std::thread::sleep(Duration::from_millis(5));
            }
            let mut guest_shutdown = false;
            loop {
                match events.try_recv() {
                    Ok(e) => {
                        if let EventKind::Shutdown { guest: true, .. } = e.kind() {
                            guest_shutdown = true;
                        }
                    }
                    Err(broadcast::error::TryRecvError::Lagged(_)) => {}
                    Err(_) => break,
                }
            }
            let starting = matches!(*state_tx.borrow(), ProcessState::Starting);
            let cause = if flags.emergency.load(Ordering::SeqCst) {
                ExitCause::EmergencyStop
            } else if guest_shutdown {
                ExitCause::GuestShutdown
            } else if flags.quit_requested.load(Ordering::SeqCst) {
                ExitCause::QmpQuit
            } else if starting {
                ExitCause::StartupFailed
            } else {
                ExitCause::Unexpected
            };
            remove_sockets(&cmd);
            state_tx.send_replace(ProcessState::Exited { code, cause });
        })
        .map_err(BackendError::Io)?;
    Ok(())
}

/// Arresto di emergenza: percorso diretto verso le primitive OS, utilizzabile
/// da qualsiasi thread senza runtime asincrono e senza passare da QMP
/// (ADR-0009).
#[derive(Clone, Debug)]
pub struct EmergencyStop {
    container: Arc<dyn ProcessContainer>,
    child: Arc<ChildProcess>,
    flags: Arc<RunFlags>,
}

impl EmergencyStop {
    /// Richiede la terminazione del contenitore e ritorna subito.
    pub fn trigger(&self) -> Result<()> {
        self.flags.emergency.store(true, Ordering::SeqCst);
        self.container.kill_all(EMERGENCY_EXIT_CODE)
    }

    /// Termina e attende l'uscita osservata del processo e lo svuotamento del
    /// contenitore. Il completamento è dichiarato solo dopo entrambi.
    pub fn trigger_and_wait(&self, timeout: Duration) -> Result<(Option<u32>, Duration)> {
        let t0 = Instant::now();
        if self.child.try_exit_code()?.is_none() {
            self.trigger()?;
        }
        let code = self.child.wait(Some(timeout))?;
        if code.is_none() {
            return Err(BackendError::Timeout("uscita di QEMU dopo Emergency Stop"));
        }
        while self.container.active_count()? > 0 {
            if t0.elapsed() > timeout {
                return Err(BackendError::Timeout(
                    "svuotamento del contenitore dopo Emergency Stop",
                ));
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        Ok((code, t0.elapsed()))
    }

    /// Identità del processo controllato.
    pub fn identity(&self) -> ProcessIdentity {
        self.child.identity()
    }
}

/// `true` se la causa d'uscita è un arresto ordinato (vedi
/// [`QemuRun::observation`]): `quit`, Emergency Stop o spegnimento del guest
/// segnalato da QMP. Avvio fallito e uscita inattesa non lo sono.
pub fn exit_cause_expected(cause: ExitCause) -> bool {
    match cause {
        ExitCause::EmergencyStop | ExitCause::QmpQuit | ExitCause::GuestShutdown => true,
        ExitCause::StartupFailed | ExitCause::Unexpected => false,
    }
}

/// Verifica di assenza di NIC sui dati di `info network` e `query-pci`
/// (F0-02 §5: `query-rx-filter` non è un indicatore affidabile).
pub fn check_no_nic(info_network: &str, pci_network: &[PciNetworkDevice]) -> Result<()> {
    let info = info_network.trim();
    if !info.is_empty() || !pci_network.is_empty() {
        return Err(BackendError::UnexpectedNetwork(format!(
            "info network={info:?}, PCI di classe rete={pci_network:?}"
        )));
    }
    Ok(())
}

/// Esecuzione di una VM.
#[derive(Debug)]
pub struct QemuRun {
    run_id: RunId,
    identity: ProcessIdentity,
    qmp: QmpClient,
    state_tx: Arc<watch::Sender<ProcessState>>,
    emergency: EmergencyStop,
    private_dir: ManagedPath,
    command: QemuCommand,
    geometry: Mutex<Option<DisplayGeometry>>,
    pressed: Mutex<PressedInput>,
    accelerator: AcceleratorsInfo,
    cfg: RunConfig,
    guards: StorageGuards,
}

impl QemuRun {
    /// Avvia una VM secondo `spec`, con percorsi da `paths`. Ritorna solo con
    /// processo vivo, QMP pronto, acceleratore e assenza di NIC verificati.
    pub async fn start(
        caps: &HostCapabilities,
        spec: &VmLaunchSpec,
        paths: &dyn ManagedPaths,
        cfg: RunConfig,
    ) -> Result<Self> {
        match &caps.accelerator {
            AcceleratorStatus::Available { accelerator, .. }
                if *accelerator == spec.accelerator => {}
            AcceleratorStatus::Available { accelerator, .. } => {
                return Err(BackendError::AcceleratorUnavailable(
                    AcceleratorStatus::Unavailable {
                        accelerator: spec.accelerator,
                        reason: UnavailableReason::ProbeFailed {
                            detail: format!(
                                "il probe ha verificato {} e non {}",
                                accelerator.qemu_name(),
                                spec.accelerator.qemu_name()
                            ),
                        },
                    },
                ));
            }
            AcceleratorStatus::Unavailable { .. } => {
                return Err(BackendError::AcceleratorUnavailable(
                    caps.accelerator.clone(),
                ));
            }
            AcceleratorStatus::NotProbed => {
                return Err(BackendError::StateConflict(
                    "acceleratore non verificato: eseguire prima il probe".into(),
                ));
            }
        }
        caps.machine(spec.machine.as_str())?;
        caps.cpu_model(spec.cpu.as_str())?;
        if !caps.missing_devices.is_empty() {
            return Err(BackendError::Unsupported {
                what: format!("dispositivi {}", caps.missing_devices.join(", ")),
                hint: "il binario QEMU non include i dispositivi richiesti".into(),
            });
        }
        let command = spec.build(paths)?;
        if command.program != caps.qemu_system {
            return Err(BackendError::StateConflict(
                "il binario QEMU dell'esecuzione non è quello verificato dal probe".into(),
            ));
        }
        let run = &spec.run_id;
        let readonly = paths.protected_readonly(run)?;
        let mut no_delete = vec![paths.firmware_vars(run)?];
        if let Some(d) = paths.disk(run)? {
            no_delete.push(d);
        }
        let guards = StorageGuards::acquire(&readonly, &no_delete)?;

        let launched = launch(&command, &cfg.qmp, cfg.startup_timeout).await?;
        let identity = launched.child.identity();
        let emergency = EmergencyStop {
            container: launched.container.clone(),
            child: launched.child.clone(),
            flags: launched.flags.clone(),
        };
        let run = Self {
            run_id: spec.run_id,
            identity,
            qmp: launched.qmp,
            state_tx: launched.state_tx,
            emergency,
            private_dir: command.private_dir.clone(),
            command,
            geometry: Mutex::new(None),
            pressed: Mutex::new(PressedInput::default()),
            accelerator: AcceleratorsInfo {
                enabled: String::new(),
                present: vec![],
            },
            cfg,
            guards,
        };
        match run.post_start_checks(spec).await {
            Ok(acc) => {
                let mut run = run;
                run.accelerator = acc;
                if !run.cfg.leave_paused {
                    run.qmp.cont().await?;
                }
                run.state_tx.send_if_modified(|s| {
                    if matches!(s, ProcessState::Starting) {
                        *s = ProcessState::Running;
                        true
                    } else {
                        false
                    }
                });
                if let ProcessState::Exited { code, .. } = run.state() {
                    return Err(BackendError::StartupFailed {
                        exit_code: code,
                        stderr_tail: read_stderr_tail(&run.command.stderr),
                    });
                }
                Ok(run)
            }
            Err(e) => {
                let em = run.emergency.clone();
                let _ = tokio::task::spawn_blocking(move || {
                    em.trigger_and_wait(Duration::from_secs(10))
                })
                .await;
                Err(e)
            }
        }
    }

    async fn post_start_checks(&self, spec: &VmLaunchSpec) -> Result<AcceleratorsInfo> {
        let acc = self.qmp.query_accelerators().await?;
        if acc.enabled != spec.accelerator.qemu_name() {
            return Err(BackendError::AcceleratorUnavailable(
                AcceleratorStatus::Unavailable {
                    accelerator: spec.accelerator,
                    reason: UnavailableReason::WrongAccelerator {
                        enabled: acc.enabled,
                    },
                },
            ));
        }
        self.verify_no_nic().await?;
        Ok(acc)
    }

    /// Verifica che il guest non abbia interfacce di rete: `info network`
    /// vuoto e nessun dispositivo PCI di classe rete.
    pub async fn verify_no_nic(&self) -> Result<()> {
        let info = self.qmp.network_diagnostics().await?;
        let pci = self.qmp.pci_network_devices().await?;
        check_no_nic(&info, &pci)
    }

    /// Identificativo dell'esecuzione.
    pub fn run_id(&self) -> RunId {
        self.run_id
    }

    /// Identità del processo QEMU (PID, tempo di creazione, `run_id`).
    pub fn identity(&self) -> ProcessIdentity {
        self.identity
    }

    /// Acceleratore verificato all'avvio.
    pub fn accelerator(&self) -> &AcceleratorsInfo {
        &self.accelerator
    }

    /// Comando usato (diagnostica).
    pub fn command(&self) -> &QemuCommand {
        &self.command
    }

    /// File protetti dall'esclusione durante l'esecuzione.
    pub fn storage_guards(&self) -> &StorageGuards {
        &self.guards
    }

    /// Stato corrente del processo.
    pub fn state(&self) -> ProcessState {
        self.state_tx.borrow().clone()
    }

    /// Osservatore dello stato del processo (rileva anche le uscite inattese).
    pub fn subscribe_state(&self) -> watch::Receiver<ProcessState> {
        self.state_tx.subscribe()
    }

    /// Eventi QMP.
    pub fn subscribe_events(&self) -> broadcast::Receiver<QmpEvent> {
        self.qmp.subscribe_events()
    }

    /// Maniglia di Emergency Stop, clonabile e indipendente dalla coda QMP.
    pub fn emergency_stop_handle(&self) -> EmergencyStop {
        self.emergency.clone()
    }

    /// Processi attivi nel contenitore dell'esecuzione.
    pub fn active_processes(&self) -> Result<u32> {
        self.emergency.container.active_count()
    }

    /// Osservazione per il core (`vm-core`) corrispondente allo stato corrente.
    pub fn observation(&self) -> BackendObservation {
        let run_id = self.run_id;
        match self.state() {
            ProcessState::Starting => BackendObservation::ProcessSpawned { run_id },
            ProcessState::Running | ProcessState::Stopping => {
                BackendObservation::QmpReady { run_id }
            }
            ProcessState::Exited {
                cause: ExitCause::StartupFailed,
                ..
            } => BackendObservation::StartFailed { run_id },
            ProcessState::Exited { cause, .. } => BackendObservation::ProcessExited {
                run_id,
                expected: self.exit_expected(cause),
            },
        }
    }

    /// Uscita attesa: richiesta dal backend (quit, Emergency Stop) oppure
    /// spegnimento avviato dal guest e segnalato da QMP (`SHUTDOWN` con
    /// `guest: true`), anche senza una richiesta di arresto del backend: è uno
    /// spegnimento ordinato, non un guasto (proposta di `vm-core`).
    fn exit_expected(&self, cause: ExitCause) -> bool {
        exit_cause_expected(cause)
    }

    /// Riconcilia lo stato da handle del processo e QMP. L'handle è
    /// autoritativo sulla vita del processo; QMP sullo stato delle vCPU.
    pub async fn reconcile(&self, qmp_timeout: Duration) -> Result<Reconciliation> {
        let handle_exit_code = self.emergency.child.try_exit_code()?;
        let qmp = self.qmp.state();
        let qmp_status = if handle_exit_code.is_none() && qmp == ConnectionState::Ready {
            self.qmp
                .query_status_within(qmp_timeout)
                .await
                .ok()
                .map(|s| s.status)
        } else {
            None
        };
        let process = self.state();
        let mut anomaly = None;
        let (state, source) = if handle_exit_code.is_some() {
            let cause = match &process {
                ProcessState::Exited { cause, .. } => Some(*cause),
                _ => None,
            };
            match cause {
                Some(c) if self.exit_expected(c) => (VmState::Stopped, ObservationSource::Process),
                Some(_) => (VmState::Failed, ObservationSource::Process),
                // Uscita già osservata sull'handle, classificazione in corso.
                None => (VmState::Stopping, ObservationSource::Process),
            }
        } else {
            match (&process, &qmp, &qmp_status) {
                (ProcessState::Starting, ..) => (VmState::Starting, ObservationSource::Process),
                (ProcessState::Stopping, ..) => (VmState::Stopping, ObservationSource::Process),
                (_, ConnectionState::Closed { reason }, _) => {
                    anomaly = Some(format!("processo vivo ma canale QMP chiuso: {reason}"));
                    (VmState::Failed, ObservationSource::Qmp)
                }
                (_, _, None) => {
                    anomaly = Some("processo vivo ma QMP non risponde".into());
                    (VmState::Failed, ObservationSource::Qmp)
                }
                (_, _, Some(s)) => {
                    match s {
                        RunState::Running | RunState::Paused | RunState::Prelaunch => {}
                        other => anomaly = Some(format!("stato delle vCPU {other:?}")),
                    }
                    (VmState::Running, ObservationSource::Qmp)
                }
            }
        };
        Ok(Reconciliation {
            process,
            handle_exit_code,
            qmp,
            qmp_status,
            observed: ObservedState {
                state,
                source,
                run_id: Some(self.run_id),
                observed_at: UtcTimestamp::now(),
            },
            anomaly,
        })
    }

    fn ensure_running(&self) -> Result<()> {
        match self.state() {
            ProcessState::Running => Ok(()),
            s => Err(BackendError::StateConflict(format!(
                "operazione non ammessa nello stato {s:?}"
            ))),
        }
    }

    fn ensure_run(&self, run_id: RunId) -> Result<()> {
        if run_id != self.run_id {
            return Err(BackendError::StateConflict(format!(
                "richiesta riferita all'esecuzione {run_id}, attiva {}",
                self.run_id
            )));
        }
        Ok(())
    }

    /// `query-status`.
    pub async fn status(&self) -> Result<StatusInfo> {
        Ok(self.qmp.query_status().await?)
    }

    /// `query-version`.
    pub async fn qemu_version(&self) -> Result<VersionInfo> {
        Ok(self.qmp.query_version().await?)
    }

    /// Macchina corrente.
    pub async fn current_machine(&self) -> Result<CurrentMachine> {
        Ok(self.qmp.query_current_machine().await?)
    }

    /// `query-name`.
    pub async fn guest_name(&self) -> Result<Option<String>> {
        Ok(self.qmp.query_name().await?)
    }

    /// Diagnostica di rete (`info network`).
    pub async fn network_diagnostics(&self) -> Result<String> {
        Ok(self.qmp.network_diagnostics().await?)
    }

    /// `system_reset`.
    pub async fn reset(&self) -> Result<()> {
        self.ensure_running()?;
        Ok(self.qmp.system_reset().await?)
    }

    /// Screenshot via `screendump` (file deciso dal backend nella directory
    /// privata, rimosso in ogni caso) con il limite di [`RunConfig`].
    pub async fn screenshot(&self) -> Result<CapturedScreenshot> {
        self.screenshot_limited(self.cfg.max_screenshot_bytes).await
    }

    /// Screenshot con limite esplicito sulla dimensione del PNG.
    pub async fn screenshot_limited(&self, max_png_bytes: u64) -> Result<CapturedScreenshot> {
        Ok(self.screenshot_rgb_limited(max_png_bytes).await?.0)
    }

    /// Screenshot con l'immagine RGB decodificata (diagnostica e test).
    pub async fn screenshot_rgb(&self) -> Result<(CapturedScreenshot, RgbImage)> {
        self.screenshot_rgb_limited(self.cfg.max_screenshot_bytes)
            .await
    }

    async fn screenshot_rgb_limited(
        &self,
        max_png_bytes: u64,
    ) -> Result<(CapturedScreenshot, RgbImage)> {
        self.ensure_running()?;
        let r = crate::screenshot::capture(
            &self.qmp,
            &self.private_dir,
            self.run_id,
            self.cfg.screendump_timeout,
            max_png_bytes,
        )
        .await?;
        *lock(&self.geometry) = Some(r.0.geometry);
        Ok(r)
    }

    /// Geometria dell'ultimo screenshot, se nota.
    pub fn current_geometry(&self) -> Option<DisplayGeometry> {
        *lock(&self.geometry)
    }

    /// Inoltra eventi di input. La richiesta deve riferirsi a questa
    /// esecuzione e alla geometria dell'ultimo screenshot, se nota, per
    /// rifiutare coordinate obsolete (ROADMAP 6.4). Nessuna ripetizione
    /// automatica.
    pub async fn send_input(
        &self,
        run_id: RunId,
        geometry: DisplayGeometry,
        events: &[InputEvent],
    ) -> Result<()> {
        self.ensure_run(run_id)?;
        self.ensure_running()?;
        validate_events(geometry, events)?;
        if let Some(g) = *lock(&self.geometry)
            && g != geometry
        {
            return Err(BackendError::StateConflict(format!(
                "geometria {}x{} obsoleta (attuale {}x{})",
                geometry.width, geometry.height, g.width, g.height
            )));
        }
        lock(&self.pressed).apply(events);
        Ok(self.qmp.input_send_event(events).await?)
    }

    /// Lotto di input di `lightvm-proto` (validazione strutturale inclusa).
    pub async fn send_input_batch(&self, batch: &lightvm_proto::InputBatch) -> Result<()> {
        batch.validate()?;
        self.send_input(batch.run_id, batch.geometry, &batch.events)
            .await
    }

    /// `true` se risultano tasti o pulsanti premuti.
    pub fn has_pressed_input(&self) -> bool {
        !lock(&self.pressed).is_empty()
    }

    /// Rilascia tutti i tasti e pulsanti risultanti premuti (perdita di focus,
    /// disconnessione, cambio proprietario dell'input: ROADMAP 6.4).
    /// Restituisce il numero di eventi di rilascio inviati.
    pub async fn release_all_input(&self, run_id: RunId) -> Result<usize> {
        self.ensure_run(run_id)?;
        let events = lock(&self.pressed).take_release_events();
        if events.is_empty() || !matches!(self.state(), ProcessState::Running) {
            return Ok(0);
        }
        self.qmp.input_send_event(&events).await?;
        Ok(events.len())
    }

    /// Attende l'uscita del processo fino a `timeout`.
    pub async fn wait_exit(&self, timeout: Duration) -> Option<(Option<u32>, ExitCause)> {
        let mut rx = self.state_tx.subscribe();
        let r = tokio::time::timeout(
            timeout,
            rx.wait_for(|s| matches!(s, ProcessState::Exited { .. })),
        )
        .await;
        match r {
            Ok(Ok(s)) => match &*s {
                ProcessState::Exited { code, cause } => Some((*code, *cause)),
                _ => None,
            },
            _ => None,
        }
    }

    fn mark_stopping(&self) {
        self.emergency
            .flags
            .stop_requested
            .store(true, Ordering::SeqCst);
        self.state_tx.send_if_modified(|s| {
            if matches!(s, ProcessState::Starting | ProcessState::Running) {
                *s = ProcessState::Stopping;
                true
            } else {
                false
            }
        });
    }

    fn report(&self, phase: StopPhase, t0: Instant) -> Result<ExitReport> {
        self.report_with(phase, t0, Vec::new())
    }

    fn report_with(&self, phase: StopPhase, t0: Instant, notes: Vec<String>) -> Result<ExitReport> {
        match self.state() {
            ProcessState::Exited { code, cause } => Ok(ExitReport {
                code,
                class: classify_exit_code(code),
                cause,
                phase,
                elapsed: t0.elapsed(),
                active_after: self.emergency.container.active_count()?,
                notes,
            }),
            _ => Err(BackendError::Timeout("uscita di QEMU")),
        }
    }

    /// Arresto gentile con scalata: `system_powerdown` → attesa → `quit` →
    /// attesa → terminazione del contenitore (`TerminateJobObject`). Ritorna
    /// solo dopo l'uscita osservata sull'handle.
    pub async fn shutdown(&self, policy: ShutdownPolicy) -> Result<ExitReport> {
        let t0 = Instant::now();
        if matches!(self.state(), ProcessState::Exited { .. }) {
            return self.report(StopPhase::AlreadyExited, t0);
        }
        self.mark_stopping();
        let mut notes = Vec::new();
        // Ogni fase dura al massimo il proprio timeout, comando QMP compreso:
        // un QEMU che non risponde non allunga la scalata.
        if !policy.powerdown_timeout.is_zero() {
            let deadline = Instant::now() + policy.powerdown_timeout;
            if let Err(e) = self
                .qmp
                .system_powerdown_within(policy.powerdown_timeout)
                .await
            {
                notes.push(format!("system_powerdown: {}", e.public_summary()));
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if self.wait_exit(left).await.is_some() {
                return self.report_with(StopPhase::Powerdown, t0, notes);
            }
            notes.push("nessuna uscita dopo system_powerdown".into());
        }
        self.emergency
            .flags
            .quit_requested
            .store(true, Ordering::SeqCst);
        let deadline = Instant::now() + policy.quit_timeout;
        if let Err(e) = self.qmp.quit_within(policy.quit_timeout).await {
            notes.push(format!("quit: {}", e.public_summary()));
        }
        let left = deadline.saturating_duration_since(Instant::now());
        if self.wait_exit(left).await.is_some() {
            return self.report_with(StopPhase::Quit, t0, notes);
        }
        notes.push("nessuna uscita dopo quit: terminazione del Job Object".into());
        self.emergency.trigger()?;
        if self.wait_exit(policy.kill_timeout).await.is_some() {
            return self.report_with(StopPhase::Kill, t0, notes);
        }
        Err(BackendError::Timeout("arresto di QEMU"))
    }

    /// Emergency Stop: terminazione del contenitore senza QMP, completata
    /// solo dopo l'uscita osservata e lo svuotamento del contenitore.
    pub async fn emergency_stop(&self, timeout: Duration) -> Result<ExitReport> {
        let t0 = Instant::now();
        self.mark_stopping();
        let em = self.emergency.clone();
        tokio::task::spawn_blocking(move || em.trigger_and_wait(timeout))
            .await
            .map_err(|e| BackendError::Io(std::io::Error::other(e.to_string())))??;
        if self.wait_exit(timeout).await.is_none() {
            return Err(BackendError::Timeout("stato finale dopo Emergency Stop"));
        }
        self.report(StopPhase::Kill, t0)
    }

    /// Chiude l'esecuzione: client QMP, socket, handle dei file protetti e, se
    /// nessun [`EmergencyStop`] clonato è ancora vivo, il Job Object. Se il
    /// processo è ancora vivo viene prima eseguito l'Emergency Stop
    /// (`forced = true` nel rapporto): chiudere significa perdere il controllo.
    pub async fn close(self) -> Result<CloseReport> {
        let forced = !matches!(self.state(), ProcessState::Exited { .. });
        if forced {
            self.emergency_stop(Duration::from_secs(10)).await?;
        }
        let active_at_close = self.emergency.container.active_count()?;
        self.qmp.close(Duration::from_millis(500)).await;
        let identity = self.identity;
        let qmp_socket = self.command.qmp_socket.clone();
        remove_sockets(&self.command);
        // L'unico riferimento rimasto all'handle del processo è quello dell'esecuzione
        // (il thread di attesa lo rilascia prima di pubblicare l'uscita).
        let process_handle_released = Arc::strong_count(&self.emergency.child) == 1;
        let container = self.emergency.container.clone();
        drop(self);
        // Unico riferimento rimasto: rilasciarlo chiude l'handle del Job Object.
        let job_closed = Arc::strong_count(&container) == 1;
        drop(container);
        Ok(CloseReport {
            identity,
            forced,
            active_at_close,
            job_closed,
            process_handle_released,
            qmp_socket_left: qmp_socket.as_path().exists(),
        })
    }
}

impl Drop for QemuRun {
    fn drop(&mut self) {
        // Perdere l'oggetto significa perdere il controllo dell'esecuzione:
        // il processo viene terminato (lo farebbe comunque la chiusura del
        // Job Object con l'ultimo riferimento al contenitore).
        if !matches!(self.state(), ProcessState::Exited { .. }) {
            let _ = self.emergency.trigger();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verifica_nic() {
        assert!(check_no_nic("", &[]).is_ok());
        assert!(check_no_nic("  \r\n", &[]).is_ok());
        let e = check_no_nic("hub 0\r\n \\ net0: index=0,type=user", &[]).unwrap_err();
        assert!(matches!(e, BackendError::UnexpectedNetwork(_)));
        let pci = [PciNetworkDevice {
            bus: 0,
            slot: 2,
            description: "Ethernet controller".into(),
        }];
        assert!(check_no_nic("", &pci).is_err());
    }

    #[test]
    fn uscite_attese() {
        assert!(exit_cause_expected(ExitCause::GuestShutdown));
        assert!(exit_cause_expected(ExitCause::QmpQuit));
        assert!(exit_cause_expected(ExitCause::EmergencyStop));
        assert!(!exit_cause_expected(ExitCause::Unexpected));
        assert!(!exit_cause_expected(ExitCause::StartupFailed));
    }
}

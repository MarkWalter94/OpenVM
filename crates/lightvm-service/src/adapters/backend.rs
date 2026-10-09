//! Adattatori del backend QEMU (`qemu-backend`):
//!
//! - [`QemuVmBackend`]: porta `vm_core::ports::VmBackend` (più le osservazioni
//!   di [`BackendPort`]) su [`qemu_backend::QemuRun`]; vive sul thread del
//!   supervisor e usa un proprio runtime tokio per le API asincrone del backend;
//! - [`QemuHost`]: host del servizio ([`crate::supervisor::Backend`]) con il
//!   percorso prioritario dell'Emergency Stop: un registro condiviso dei
//!   gestori [`qemu_backend::EmergencyStop`] (Job Object su Windows), usabile
//!   da qualunque thread senza passare dal supervisor né da QMP (ADR-0009);
//! - [`UnavailableBackend`] / [`UnavailableHost`]: QEMU non configurato; il
//!   core resta reale e l'avvio è rifiutato (`unsupported`) con audit.
//!
//! Lacune dell'API attuale, coperte qui nel modo più sottile possibile e
//! riportate nel file di verifica F1-04:
//! - nessuna creazione del disco principale (`qemu-img create`) né sua
//!   registrazione nello storage: le VM partono senza disco (`disk: None`);
//! - le directory dell'esecuzione e la directory privata dei socket sono create
//!   qui con l'ACL ereditata (l'ACL esplicita solo-utente è F1-06/F1-09);
//! - la copia per esecuzione delle variabili UEFI è fatta qui dal modello del
//!   firmware della distribuzione QEMU (lo storage non la produce ancora);
//! - screenshot e input non sono collegati (le capacità host li dichiarano
//!   assenti, quindi il core non li inoltra).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use lightvm_proto::ipc::StopReason;
use lightvm_proto::{
    BackendObservation, DisplayGeometry, HostCapacity, InputEvent, MediaId, PublicError, RunId,
    Screenshot, VmId, VmRecord,
};
use qemu_backend::probe::{ProbeConfig, probe_accelerator, probe_host};
use qemu_backend::{
    Accelerator, BackendError, CpuModel, ExitCause, HostCapabilities, MachineType, ManagedPath,
    ManagedPaths, ProcessState, QemuRun, RunConfig, ShutdownPolicy, VmLaunchSpec,
};
use vm_core::media::VerifiedMedia;
use vm_core::ports::VmBackend;

use crate::engine::BackendPort;
use crate::supervisor::{Backend, EmergencyOutcome, EmergencyStop, OrphanPolicy};

type Registry = Arc<Mutex<BTreeMap<VmId, qemu_backend::EmergencyStop>>>;

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Errore pubblico del backend; il dettaglio (con percorsi) resta su stderr.
fn public(e: &BackendError) -> PublicError {
    crate::diag!("lightvm-service: backend QEMU: {e}");
    e.to_public()
}

fn io_public(what: &str, e: &std::io::Error) -> PublicError {
    crate::diag!("lightvm-service: backend QEMU: {what}: {e}");
    PublicError::infrastructure(format!("backend QEMU: {what}"))
}

/// Nome dell'eseguibile QEMU nella distribuzione approvata.
const QEMU_SYSTEM: &str = if cfg!(windows) {
    "qemu-system-x86_64.exe"
} else {
    "qemu-system-x86_64"
};
const QEMU_IMG: &str = if cfg!(windows) {
    "qemu-img.exe"
} else {
    "qemu-img"
};

/// Configurazione del backend QEMU reale.
#[derive(Debug, Clone)]
pub struct QemuSetup {
    /// Directory della distribuzione QEMU approvata (binari e `share/`).
    pub qemu_dir: PathBuf,
    /// Radice del catalogo (directory `runs/` e copie gestite dei media).
    pub catalog_root: PathBuf,
    /// Radice delle directory private per i socket QMP/VNC: deve essere corta
    /// (limite di 107 byte del percorso di un socket Unix).
    pub private_root: PathBuf,
    /// Modello CPU nominale (F0: `qemu64`; F1-17 valuta modelli più ricchi).
    pub cpu_model: String,
    /// Politica dell'arresto ordinario (`system_powerdown` → `quit` → kill).
    pub stop_policy: ShutdownPolicy,
    /// Attesa massima dell'Emergency Stop e dell'arresto forzato.
    pub emergency_timeout: Duration,
    /// Configurazione delle esecuzioni.
    pub run_config: RunConfig,
    /// Configurazione del probe (hash dei binari approvati, timeout).
    pub probe: ProbeConfig,
    /// Spazio libero dichiarato per il catalogo in GiB.
    ///
    /// TODO(F1): nessuna API di probe dello spazio libero in `qemu-backend` o
    /// `vm-storage`; il valore limita soltanto `disk_gib` alla creazione.
    pub free_storage_gib: u64,
}

impl QemuSetup {
    /// Valori predefiniti per una distribuzione e un catalogo.
    pub fn new(qemu_dir: PathBuf, catalog_root: PathBuf, private_root: PathBuf) -> Self {
        Self {
            qemu_dir,
            catalog_root,
            private_root,
            cpu_model: "qemu64".into(),
            stop_policy: ShutdownPolicy::default(),
            emergency_timeout: Duration::from_secs(10),
            run_config: RunConfig::default(),
            probe: ProbeConfig::default(),
            free_storage_gib: 256,
        }
    }
}

/// Memoria fisica totale in MiB, se rilevabile.
fn total_memory_mib() -> Option<u64> {
    #[cfg(windows)]
    {
        let mut s = sysinfo::System::new();
        s.refresh_memory();
        let b = s.total_memory();
        (b > 0).then_some(b / (1024 * 1024))
    }
    #[cfg(target_os = "linux")]
    {
        let text = std::fs::read_to_string("/proc/meminfo").ok()?;
        let line = text.lines().find(|l| l.starts_with("MemTotal:"))?;
        let kib: u64 = line.split_whitespace().nth(1)?.parse().ok()?;
        Some(kib / 1024)
    }
    #[cfg(not(any(windows, target_os = "linux")))]
    {
        None
    }
}

/// Capacità host per il core a partire dal probe.
fn host_capacity(caps: &HostCapabilities, free_storage_gib: u64) -> HostCapacity {
    HostCapacity {
        logical_cpus: std::thread::available_parallelism()
            .map(|n| u32::try_from(n.get()).unwrap_or(u32::MAX))
            .unwrap_or(1),
        total_memory_mib: total_memory_mib().unwrap_or(4096),
        reserved_memory_mib: 2048,
        free_storage_gib,
        reserved_storage_gib: 16,
        accelerator_available: caps.accelerator.is_available()
            && caps.firmware.is_some()
            && caps.recommended_machine().is_some(),
        // Non collegati al servizio in F1 (vedi intestazione del modulo).
        screenshot_available: false,
        input_available: false,
        baseline_available: false,
    }
}

/// Probe dei binari approvati e dell'acceleratore; restituisce host, backend
/// delle VM e capacità host per il core.
///
/// Va chiamato fuori da un runtime tokio (usa `block_on` sul runtime proprio
/// del backend).
pub fn probe(setup: QemuSetup) -> Result<(QemuHost, QemuVmBackend, HostCapacity), PublicError> {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .thread_name("lightvm-qemu")
        .enable_all()
        .build()
        .map_err(|e| io_public("runtime", &e))?;
    let qemu_system =
        ManagedPath::verify(setup.qemu_dir.join(QEMU_SYSTEM)).map_err(|e| public(&e))?;
    let qemu_img = ManagedPath::verify(setup.qemu_dir.join(QEMU_IMG)).map_err(|e| public(&e))?;
    let mut caps = probe_host(&qemu_system, &qemu_img, &setup.probe).map_err(|e| public(&e))?;
    let machine = caps
        .recommended_machine()
        .cloned()
        .ok_or_else(|| PublicError::unsupported("nessuna macchina q35 versionata nel binario"))?;
    let cpu = caps.cpu_model(&setup.cpu_model).map_err(|e| public(&e))?;
    let probe_dir = setup.private_root.join("probe");
    std::fs::create_dir_all(&probe_dir).map_err(|e| io_public("directory del probe", &e))?;
    let probe_dir = ManagedPath::verify(probe_dir).map_err(|e| public(&e))?;
    caps.accelerator = rt.block_on(probe_accelerator(
        &caps,
        Accelerator::for_current_host(),
        &machine,
        &cpu,
        &probe_dir,
        &setup.probe,
    ));
    if !caps.accelerator.is_available() {
        crate::diag!(
            "lightvm-service: acceleratore non disponibile: {}",
            caps.accelerator.public_message()
        );
    }
    let capacity = host_capacity(&caps, setup.free_storage_gib);
    let registry: Registry = Arc::default();
    let host = QemuHost {
        version: caps.version.to_string(),
        accelerator: caps
            .accelerator
            .is_available()
            .then(|| Accelerator::for_current_host().qemu_name().to_owned()),
        registry: registry.clone(),
        emergency_timeout: setup.emergency_timeout,
    };
    let backend = QemuVmBackend {
        rt,
        caps,
        machine,
        cpu,
        setup,
        runs: BTreeMap::new(),
        registry,
    };
    Ok((host, backend, capacity))
}

/// Percorsi di un'esecuzione per `qemu-backend`, tutti decisi dal servizio.
#[derive(Debug)]
struct RunPaths {
    run_id: RunId,
    qemu_system: ManagedPath,
    firmware_code: ManagedPath,
    firmware_vars: ManagedPath,
    run_dir: ManagedPath,
    private_dir: ManagedPath,
    media: BTreeMap<MediaId, ManagedPath>,
}

impl RunPaths {
    fn check(&self, run: &RunId) -> qemu_backend::error::Result<()> {
        if *run == self.run_id {
            Ok(())
        } else {
            Err(BackendError::invalid("run_id", "esecuzione diversa"))
        }
    }
}

impl ManagedPaths for RunPaths {
    fn qemu_system(&self) -> qemu_backend::error::Result<ManagedPath> {
        Ok(self.qemu_system.clone())
    }
    fn firmware_code(&self) -> qemu_backend::error::Result<ManagedPath> {
        Ok(self.firmware_code.clone())
    }
    fn firmware_vars(&self, run: &RunId) -> qemu_backend::error::Result<ManagedPath> {
        self.check(run)?;
        Ok(self.firmware_vars.clone())
    }
    fn disk(&self, run: &RunId) -> qemu_backend::error::Result<Option<ManagedPath>> {
        self.check(run)?;
        Ok(None)
    }
    fn protected_readonly(&self, run: &RunId) -> qemu_backend::error::Result<Vec<ManagedPath>> {
        self.check(run)?;
        Ok(Vec::new())
    }
    fn media(&self, media: &MediaId) -> qemu_backend::error::Result<ManagedPath> {
        self.media
            .get(media)
            .cloned()
            .ok_or_else(|| BackendError::invalid("media", "supporto non verificato dal core"))
    }
    fn run_dir(&self, run: &RunId) -> qemu_backend::error::Result<ManagedPath> {
        self.check(run)?;
        Ok(self.run_dir.clone())
    }
    fn private_dir(&self, run: &RunId) -> qemu_backend::error::Result<ManagedPath> {
        self.check(run)?;
        Ok(self.private_dir.clone())
    }
}

/// Percorso assoluto di una copia gestita (`ManagedRelPath`, separatore `/`).
fn managed_abs(root: &Path, rel: &str) -> PathBuf {
    rel.split('/').fold(root.to_path_buf(), |p, c| p.join(c))
}

/// Backend delle VM su `qemu-backend`.
pub struct QemuVmBackend {
    rt: tokio::runtime::Runtime,
    caps: HostCapabilities,
    machine: MachineType,
    cpu: CpuModel,
    setup: QemuSetup,
    runs: BTreeMap<VmId, QemuRun>,
    registry: Registry,
}

impl std::fmt::Debug for QemuVmBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("QemuVmBackend")
            .field("machine", &self.machine)
            .field("cpu", &self.cpu)
            .field("runs", &self.runs.len())
            .finish_non_exhaustive()
    }
}

impl QemuVmBackend {
    fn run(&self, vm_id: VmId, run_id: RunId) -> Result<&QemuRun, PublicError> {
        match self.runs.get(&vm_id) {
            Some(r) if r.run_id() == run_id => Ok(r),
            Some(_) => Err(PublicError::state_conflict(
                "esecuzione diversa da quella attiva",
            )),
            None => Err(PublicError::state_conflict("nessuna esecuzione attiva")),
        }
    }

    /// Rilascia un'esecuzione terminata: QMP, socket, file protetti, Job Object.
    fn release(&mut self, vm_id: VmId) {
        lock(&self.registry).remove(&vm_id);
        if let Some(run) = self.runs.remove(&vm_id) {
            match self.rt.block_on(run.close()) {
                Ok(r) if r.active_at_close > 0 => crate::diag!(
                    "lightvm-service: {} processi ancora attivi alla chiusura dell'esecuzione",
                    r.active_at_close
                ),
                Ok(_) => {}
                Err(e) => crate::diag!("lightvm-service: chiusura dell'esecuzione: {e}"),
            }
        }
    }

    fn prepare(
        &self,
        vm: &VmRecord,
        run_id: RunId,
        media: &[VerifiedMedia],
    ) -> Result<RunPaths, PublicError> {
        let fw = self.caps.firmware.as_ref().ok_or_else(|| {
            PublicError::unsupported("firmware UEFI non trovato nella distribuzione QEMU")
        })?;
        let run_text = run_id.to_string();
        let run_dir = self.setup.catalog_root.join("runs").join(&run_text);
        std::fs::create_dir_all(&run_dir)
            .map_err(|e| io_public("directory dell'esecuzione", &e))?;
        // Ultimi 12 caratteri esadecimali (parte casuale del UUID v7): percorso corto.
        let short: String = run_text
            .chars()
            .filter(char::is_ascii_hexdigit)
            .rev()
            .take(12)
            .collect();
        let private_dir = self.setup.private_root.join(short);
        std::fs::create_dir_all(&private_dir).map_err(|e| io_public("directory privata", &e))?;
        let vars = run_dir.join("vars.fd");
        std::fs::copy(&fw.vars_template, &vars).map_err(|e| io_public("variabili UEFI", &e))?;
        let mut media_paths = BTreeMap::new();
        for m in media {
            let p = managed_abs(&self.setup.catalog_root, m.entry().managed_path.as_str());
            media_paths.insert(
                m.record().id,
                ManagedPath::verify(p).map_err(|e| public(&e))?,
            );
        }
        let _ = vm;
        Ok(RunPaths {
            run_id,
            qemu_system: self.caps.qemu_system.clone(),
            firmware_code: ManagedPath::verify(fw.code.clone()).map_err(|e| public(&e))?,
            firmware_vars: ManagedPath::verify(vars).map_err(|e| public(&e))?,
            run_dir: ManagedPath::verify(run_dir).map_err(|e| public(&e))?,
            private_dir: ManagedPath::verify(private_dir).map_err(|e| public(&e))?,
            media: media_paths,
        })
    }
}

impl VmBackend for QemuVmBackend {
    fn start(
        &mut self,
        vm: &VmRecord,
        run_id: RunId,
        media: &[VerifiedMedia],
    ) -> Result<(), PublicError> {
        if self.runs.contains_key(&vm.id) {
            return Err(PublicError::state_conflict(
                "esecuzione già attiva per la VM",
            ));
        }
        let paths = self.prepare(vm, run_id, media)?;
        let spec = VmLaunchSpec {
            run_id,
            machine: self.machine.clone(),
            accelerator: Accelerator::for_current_host(),
            cpu: self.cpu.clone(),
            vcpus: vm.config.vcpus,
            memory_mib: u32::try_from(vm.config.memory_mib)
                .map_err(|_| PublicError::resource_limit("memoria fuori intervallo"))?,
            // TODO(F1-12): disco principale non ancora creato né registrato.
            disk: None,
            install_media: vm.config.install_media,
            data_media: vm.config.data_media,
        };
        let run = self
            .rt
            .block_on(QemuRun::start(
                &self.caps,
                &spec,
                &paths,
                self.setup.run_config.clone(),
            ))
            .map_err(|e| public(&e))?;
        lock(&self.registry).insert(vm.id, run.emergency_stop_handle());
        self.runs.insert(vm.id, run);
        Ok(())
    }

    fn shutdown(&mut self, vm_id: VmId, run_id: RunId) -> Result<(), PublicError> {
        let policy = self.setup.stop_policy;
        let r = {
            let run = self.run(vm_id, run_id)?;
            self.rt.block_on(run.shutdown(policy))
        };
        match r {
            Ok(_) => {
                self.release(vm_id);
                Ok(())
            }
            Err(e) => Err(public(&e)),
        }
    }

    fn force_stop(&mut self, vm_id: VmId, run_id: RunId) -> Result<(), PublicError> {
        if !self.runs.contains_key(&vm_id) {
            // Nessun processo posseduto: nulla da terminare.
            return Ok(());
        }
        let timeout = self.setup.emergency_timeout;
        let r = {
            let run = self.run(vm_id, run_id)?;
            self.rt.block_on(run.emergency_stop(timeout))
        };
        match r {
            Ok(_) => {
                self.release(vm_id);
                Ok(())
            }
            Err(e) => Err(public(&e)),
        }
    }

    fn reset(&mut self, vm_id: VmId, run_id: RunId) -> Result<(), PublicError> {
        let run = self.run(vm_id, run_id)?;
        self.rt.block_on(run.reset()).map_err(|e| public(&e))
    }

    fn screenshot(
        &mut self,
        _vm_id: VmId,
        _run_id: RunId,
        _max_bytes: u64,
    ) -> Result<Screenshot, PublicError> {
        Err(PublicError::unsupported(
            "screenshot non ancora collegato al servizio",
        ))
    }

    fn send_input(
        &mut self,
        _vm_id: VmId,
        _run_id: RunId,
        _geometry: DisplayGeometry,
        _events: &[InputEvent],
    ) -> Result<(), PublicError> {
        Err(PublicError::unsupported(
            "input non ancora collegato al servizio",
        ))
    }
}

impl BackendPort for QemuVmBackend {
    fn drain_observations(&mut self) -> Vec<(VmId, BackendObservation)> {
        let exited: Vec<VmId> = self
            .runs
            .iter()
            .filter(|(_, r)| match r.state() {
                // Le uscite da Emergency Stop sono confermate dal core con
                // `force_stop` dopo il percorso prioritario.
                ProcessState::Exited {
                    cause: ExitCause::EmergencyStop,
                    ..
                } => false,
                ProcessState::Exited { .. } => true,
                _ => false,
            })
            .map(|(id, _)| *id)
            .collect();
        let mut out = Vec::new();
        for vm_id in exited {
            if let Some(run) = self.runs.get(&vm_id) {
                out.push((vm_id, run.observation()));
            }
            self.release(vm_id);
        }
        out
    }
}

/// Host del servizio con backend QEMU.
#[derive(Debug)]
pub struct QemuHost {
    version: String,
    accelerator: Option<String>,
    registry: Registry,
    emergency_timeout: Duration,
}

#[derive(Debug)]
struct QemuEmergency {
    registry: Registry,
    timeout: Duration,
}

impl EmergencyStop for QemuEmergency {
    fn emergency_stop_all(&self) -> EmergencyOutcome {
        // Copia dei gestori: il registro non resta bloccato durante l'attesa.
        let handles: Vec<qemu_backend::EmergencyStop> =
            lock(&self.registry).values().cloned().collect();
        let mut out = EmergencyOutcome {
            processes_terminated: 0,
            completed: true,
        };
        for h in handles {
            match h.trigger_and_wait(self.timeout) {
                Ok(_) => out.processes_terminated += 1,
                Err(e) => {
                    crate::diag!("lightvm-service: Emergency Stop: {e}");
                    out.completed = false;
                }
            }
        }
        out
    }
}

impl Backend for QemuHost {
    fn name(&self) -> String {
        format!("qemu {}", self.version)
    }
    fn accelerator(&self) -> Option<String> {
        self.accelerator.clone()
    }
    fn emergency_handle(&self) -> Arc<dyn EmergencyStop> {
        Arc::new(QemuEmergency {
            registry: self.registry.clone(),
            timeout: self.emergency_timeout,
        })
    }
    fn stop_agent_sessions(&mut self, _reason: StopReason) {
        // Nessuna sessione agente in F1: connessioni e credenziali non UI sono
        // già revocate dal servizio; le sessioni del core arrivano con F4.
    }
    fn on_parent_lost(&mut self, _policy: OrphanPolicy) {
        // ADR-0015 parte B in sospeso: nessuna azione propria; con la politica
        // predefinita segue l'arresto controllato (VM manuali arrestate).
    }
    fn shutdown(&mut self) {
        // Il motore ha già arrestato le VM; restano solo esecuzioni non
        // confermate, terminate con il percorso prioritario.
        let leftover = QemuEmergency {
            registry: self.registry.clone(),
            timeout: self.emergency_timeout,
        }
        .emergency_stop_all();
        if leftover.processes_terminated > 0 {
            crate::diag!(
                "lightvm-service: {} esecuzioni terminate all'arresto",
                leftover.processes_terminated
            );
        }
    }
}

/// Backend delle VM senza QEMU configurato: ogni avvio è rifiutato.
#[derive(Debug, Default)]
pub struct UnavailableBackend;

fn not_configured() -> PublicError {
    PublicError::unsupported("backend QEMU non configurato in questo servizio")
}

impl VmBackend for UnavailableBackend {
    fn start(&mut self, _: &VmRecord, _: RunId, _: &[VerifiedMedia]) -> Result<(), PublicError> {
        Err(not_configured())
    }
    fn shutdown(&mut self, _: VmId, _: RunId) -> Result<(), PublicError> {
        Err(not_configured())
    }
    fn force_stop(&mut self, _: VmId, _: RunId) -> Result<(), PublicError> {
        // Nessun processo può esistere senza backend.
        Ok(())
    }
    fn reset(&mut self, _: VmId, _: RunId) -> Result<(), PublicError> {
        Err(not_configured())
    }
    fn screenshot(&mut self, _: VmId, _: RunId, _: u64) -> Result<Screenshot, PublicError> {
        Err(not_configured())
    }
    fn send_input(
        &mut self,
        _: VmId,
        _: RunId,
        _: DisplayGeometry,
        _: &[InputEvent],
    ) -> Result<(), PublicError> {
        Err(not_configured())
    }
}

impl BackendPort for UnavailableBackend {
    fn drain_observations(&mut self) -> Vec<(VmId, BackendObservation)> {
        Vec::new()
    }
}

/// Host senza QEMU: nessun processo da terminare.
#[derive(Debug, Default)]
pub struct UnavailableHost;

#[derive(Debug)]
struct NoProcesses;

impl EmergencyStop for NoProcesses {
    fn emergency_stop_all(&self) -> EmergencyOutcome {
        EmergencyOutcome {
            processes_terminated: 0,
            completed: true,
        }
    }
}

impl Backend for UnavailableHost {
    fn name(&self) -> String {
        "non configurato".into()
    }
    fn accelerator(&self) -> Option<String> {
        None
    }
    fn emergency_handle(&self) -> Arc<dyn EmergencyStop> {
        Arc::new(NoProcesses)
    }
    fn stop_agent_sessions(&mut self, _reason: StopReason) {}
    fn on_parent_lost(&mut self, _policy: OrphanPolicy) {}
    fn shutdown(&mut self) {}
}

/// Capacità host senza QEMU: nessun acceleratore approvato, quindi `vm.start`
/// non è nella policy effettiva (errore `unsupported`, nessun ripiego TCG).
pub fn unavailable_capacity(free_storage_gib: u64) -> HostCapacity {
    HostCapacity {
        logical_cpus: std::thread::available_parallelism()
            .map(|n| u32::try_from(n.get()).unwrap_or(u32::MAX))
            .unwrap_or(1),
        total_memory_mib: total_memory_mib().unwrap_or(4096),
        reserved_memory_mib: 2048,
        free_storage_gib,
        reserved_storage_gib: 16,
        accelerator_available: false,
        screenshot_available: false,
        input_available: false,
        baseline_available: false,
    }
}

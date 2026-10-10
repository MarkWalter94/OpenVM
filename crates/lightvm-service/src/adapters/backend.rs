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
use qemu_backend::cmdline::{DiskFormat, DiskSpec};
use qemu_backend::probe::{ProbeConfig, probe_accelerator, probe_host};
use qemu_backend::{
    Accelerator, BackendError, CpuModel, ExitCause, HostCapabilities, JobLimits, MachineType,
    ManagedPath, ManagedPaths, ProcessState, QemuRun, RunConfig, ShutdownPolicy, VmLaunchSpec,
    prepare,
};
use vm_core::media::VerifiedMedia;
use vm_core::ports::VmBackend;
use vm_storage::CatalogLayout;

use crate::engine::BackendPort;
use crate::supervisor::{Backend, EmergencyOutcome, EmergencyStop, ParentLossPolicy};

type Registry = Arc<Mutex<BTreeMap<VmId, qemu_backend::EmergencyStop>>>;

/// Politica di arresto imposta alla perdita della UI (`shutdown_vms`):
/// sostituisce quella ordinaria per gli arresti che seguono.
type StopOverride = Arc<Mutex<Option<ShutdownPolicy>>>;

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
    /// Spazio libero per il catalogo in GiB, usato solo se la misura sul
    /// volume del catalogo (`prepare::host_resources`) non riesce. Limita
    /// soltanto `disk_gib` alla creazione.
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

/// Processori logici dell'host.
fn logical_cpus() -> u32 {
    std::thread::available_parallelism()
        .map(|n| u32::try_from(n.get()).unwrap_or(u32::MAX))
        .unwrap_or(1)
}

/// Limiti del Job Object dai limiti della VM (ADR-0017):
/// - tetto CPU: `cpu_max_percent` della capacità dei vCPU assegnati, espresso
///   come quota dell'intera macchina in centesimi di punto percentuale
///   (`vcpus × percento × 100 / processori logici`); 100 % = nessun tetto;
/// - memoria del job: RAM guest più `memory_margin_mib`.
pub fn job_limits(config: &lightvm_proto::VmConfig, logical: u32) -> JobLimits {
    let l = config.limits;
    let cpu_rate = (l.cpu_max_percent < 100).then(|| {
        let rate = u64::from(config.vcpus) * u64::from(l.cpu_max_percent) * 100
            / u64::from(logical.max(1));
        u32::try_from(rate.clamp(1, 10_000)).unwrap_or(10_000)
    });
    let job_memory_bytes = config
        .memory_mib
        .checked_add(u64::from(l.memory_margin_mib))
        .and_then(|m| m.checked_mul(1 << 20));
    JobLimits {
        cpu_rate,
        job_memory_bytes,
    }
}

/// Byte occupati dai file sotto `dir` (ricorsivo, senza seguire collegamenti),
/// escluso `escluso`.
fn dir_usage(dir: &Path, escluso: &Path) -> u64 {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return 0;
    };
    rd.filter_map(Result::ok)
        .map(|e| {
            let p = e.path();
            match std::fs::symlink_metadata(&p) {
                Ok(m) if m.is_dir() => dir_usage(&p, escluso),
                Ok(m) if m.is_file() && p != escluso => m.len(),
                _ => 0,
            }
        })
        .sum()
}

/// Capacità host per il core a partire dal probe.
fn host_capacity(caps: &HostCapabilities, free_storage_gib: u64) -> HostCapacity {
    HostCapacity {
        logical_cpus: logical_cpus(),
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
    let free_storage_gib = ManagedPath::verify(setup.catalog_root.clone())
        .and_then(|root| prepare::host_resources(&root))
        .map(|r| r.free_disk_bytes >> 30)
        .unwrap_or_else(|e| {
            crate::diag!(
                "lightvm-service: spazio libero del catalogo non misurato ({e}): uso {} GiB",
                setup.free_storage_gib
            );
            setup.free_storage_gib
        });
    let capacity = host_capacity(&caps, free_storage_gib);
    let registry: Registry = Arc::default();
    let stop_override: StopOverride = Arc::default();
    let host = QemuHost {
        version: caps.version.to_string(),
        accelerator: caps
            .accelerator
            .is_available()
            .then(|| Accelerator::for_current_host().qemu_name().to_owned()),
        registry: registry.clone(),
        emergency_timeout: setup.emergency_timeout,
        stop_override: stop_override.clone(),
    };
    let backend = QemuVmBackend {
        rt,
        caps,
        machine,
        cpu,
        setup,
        runs: BTreeMap::new(),
        registry,
        stop_override,
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
    disk: ManagedPath,
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
        Ok(Some(self.disk.clone()))
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
    stop_override: StopOverride,
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

    /// Disco principale della VM (`vms/<vm_id>/primary.qcow2`, disposizione
    /// di `vm-storage`): creato con `qemu-img` al primo avvio con la
    /// dimensione di `disk_gib`, poi riusato. La creazione non passa ancora dal
    /// journal del catalogo (F3): un file preesistente è usato così com'è.
    fn primary_disk(&self, vm: &VmRecord) -> Result<ManagedPath, PublicError> {
        let path = managed_abs(
            &self.setup.catalog_root,
            CatalogLayout::primary_disk(&vm.id).as_str(),
        );
        let disk = ManagedPath::verify(&path).map_err(|e| public(&e))?;
        let size_mib = vm
            .config
            .disk_gib
            .checked_mul(1024)
            .ok_or_else(|| PublicError::resource_limit("disco fuori intervallo"))?;
        if std::fs::symlink_metadata(&path).is_ok() {
            // Disco esistente: portato alla dimensione configurata se è stata
            // aumentata nelle impostazioni della VM (mai ridotto).
            if prepare::grow_disk(
                &self.caps.qemu_img,
                &disk,
                DiskFormat::Qcow2,
                size_mib,
                prepare::QEMU_IMG_TIMEOUT,
            )
            .map_err(|e| public(&e))?
            {
                crate::diag!(
                    "lightvm-service: disco della VM ingrandito a {} GiB",
                    vm.config.disk_gib
                );
            }
            return Ok(disk);
        }
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| io_public("directory della VM", &e))?;
        }
        prepare::create_disk(
            &self.caps.qemu_img,
            &disk,
            DiskFormat::Qcow2,
            size_mib,
            prepare::QEMU_IMG_TIMEOUT,
        )
        .map_err(|e| public(&e))?;
        Ok(disk)
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
        let disk = self.primary_disk(vm)?;
        Ok(RunPaths {
            run_id,
            qemu_system: self.caps.qemu_system.clone(),
            firmware_code: ManagedPath::verify(fw.code.clone()).map_err(|e| public(&e))?,
            firmware_vars: ManagedPath::verify(vars).map_err(|e| public(&e))?,
            disk,
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
        if let Some(budget) = vm.config.limits.storage_budget_gib {
            let vm_dir = managed_abs(
                &self.setup.catalog_root,
                CatalogLayout::vm_dir(&vm.id).as_str(),
            );
            let primary = managed_abs(
                &self.setup.catalog_root,
                CatalogLayout::primary_disk(&vm.id).as_str(),
            );
            let used = dir_usage(&vm_dir, &primary);
            if used > budget << 30 {
                return Err(
                    PublicError::resource_limit("budget di storage della VM superato")
                        .with_detail("used_mib", (used >> 20).to_string())
                        .with_detail("budget_gib", budget.to_string()),
                );
            }
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
            disk: Some(DiskSpec {
                format: DiskFormat::Qcow2,
            }),
            install_media: vm.config.install_media,
            data_media: vm.config.data_media,
            limits: job_limits(&vm.config, logical_cpus()),
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
        let policy = lock(&self.stop_override).unwrap_or(self.setup.stop_policy);
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
                if let Ok(r) = run.limit_report()
                    && r.memory_limit_reached()
                {
                    crate::diag!(
                        "lightvm-service: QEMU terminato al limite di memoria della VM (picco {} MiB, limite {} MiB)",
                        r.peak_job_memory_bytes >> 20,
                        r.job_memory_bytes.unwrap_or(0) >> 20
                    );
                }
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
    stop_override: StopOverride,
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
    fn on_parent_lost(&mut self, policy: ParentLossPolicy) {
        if let ParentLossPolicy::ShutdownVms { powerdown_s } = policy {
            *lock(&self.stop_override) = Some(ShutdownPolicy {
                powerdown_timeout: Duration::from_secs(u64::from(powerdown_s)),
                quit_timeout: Duration::from_secs(5),
                kill_timeout: Duration::from_secs(10),
            });
        }
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
    fn on_parent_lost(&mut self, _policy: ParentLossPolicy) {}
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fakes::sample_config;
    use lightvm_proto::VmLimits;

    #[test]
    fn limiti_del_job_dai_limiti_della_vm() {
        let mut c = sample_config("limiti");
        c.vcpus = 2;
        c.memory_mib = 2048;
        // Predefiniti: nessun tetto CPU, memoria = RAM + 1024 MiB.
        let l = job_limits(&c, 32);
        assert_eq!(l.cpu_rate, None);
        assert_eq!(l.job_memory_bytes, Some(3072 << 20));
        // 50 % di 2 vCPU su 32 processori logici = 1 core = 3,12 % della macchina.
        c.limits = VmLimits {
            cpu_max_percent: 50,
            memory_margin_mib: 512,
            storage_budget_gib: None,
        };
        let l = job_limits(&c, 32);
        assert_eq!(l.cpu_rate, Some(312));
        assert_eq!(l.job_memory_bytes, Some(2560 << 20));
        // Mai sotto 1 né sopra il 100 % della macchina.
        c.vcpus = 1;
        c.limits.cpu_max_percent = 5;
        assert_eq!(job_limits(&c, 1024).cpu_rate, Some(1));
        c.vcpus = 64;
        c.limits.cpu_max_percent = 99;
        assert_eq!(job_limits(&c, 4).cpu_rate, Some(10_000));
    }

    #[test]
    fn uso_della_directory_esclude_il_disco_principale() {
        let d = std::env::temp_dir().join(format!(
            "lightvm-dir-usage-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos())
        ));
        std::fs::create_dir_all(d.join("overlays")).unwrap();
        std::fs::write(d.join("primary.qcow2"), vec![0u8; 4096]).unwrap();
        std::fs::write(d.join("overlays").join("a.qcow2"), vec![0u8; 1000]).unwrap();
        std::fs::write(d.join("manifest.json"), vec![0u8; 24]).unwrap();
        assert_eq!(dir_usage(&d, &d.join("primary.qcow2")), 1024);
        let _ = std::fs::remove_dir_all(&d);
    }
}

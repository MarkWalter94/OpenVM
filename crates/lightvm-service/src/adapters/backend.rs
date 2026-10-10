//! QEMU backend adapters (`qemu-backend`):
//!
//! - [`QemuVmBackend`]: the `vm_core::ports::VmBackend` port (plus the
//!   [`BackendPort`] observations) over [`qemu_backend::QemuRun`]; it lives on
//!   the supervisor thread and uses its own tokio runtime for the backend's
//!   asynchronous APIs;
//! - [`QemuHost`]: service host ([`crate::supervisor::Backend`]) with the
//!   Emergency Stop priority path: a shared registry of
//!   [`qemu_backend::EmergencyStop`] handlers (Job Object on Windows), usable
//!   from any thread without going through the supervisor or QMP (ADR-0009);
//! - [`UnavailableBackend`] / [`UnavailableHost`]: QEMU not configured; the
//!   core stays real and starting is rejected (`unsupported`) with audit.
//!
//! Gaps in the current API, covered here as thinly as possible and reported
//! in the F1-03/F1-04 verification files:
//! - the primary disk (`vms/<vm_id>/primary.qcow2`) is created or grown here
//!   with `qemu-img` on first start, without going through the catalog
//!   journal (F3);
//! - the run directories and the private socket directory are created here
//!   with the inherited ACL (the explicit user-only ACL is F1-06/F1-09); the
//!   private directory is removed when the run is released, the run directory
//!   (`runs/<run_id>/`, QEMU log and UEFI variables) stays for diagnostics and
//!   does not count towards the VM storage budget;
//! - the per-run copy of the UEFI variables is made here from the firmware
//!   template of the QEMU distribution (the storage does not produce it yet);
//! - screenshot and input are not connected (the host capabilities declare
//!   them absent, so the core does not forward them).

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

/// Stop policy imposed when the UI is lost (`shutdown_vms`): replaces the
/// ordinary one for the stops that follow.
type StopOverride = Arc<Mutex<Option<ShutdownPolicy>>>;

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Public backend error; the detail (with paths) stays on stderr.
fn public(e: &BackendError) -> PublicError {
    crate::diag!("lightvm-service: QEMU backend: {e}");
    e.to_public()
}

fn io_public(what: &str, e: &std::io::Error) -> PublicError {
    crate::diag!("lightvm-service: QEMU backend: {what}: {e}");
    PublicError::infrastructure(format!("QEMU backend: {what}"))
}

/// Name of the QEMU executable in the approved distribution.
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

/// Configuration of the real QEMU backend.
#[derive(Debug, Clone)]
pub struct QemuSetup {
    /// Directory of the approved QEMU distribution (binaries and `share/`).
    pub qemu_dir: PathBuf,
    /// Catalog root (`runs/` directory and managed media copies).
    pub catalog_root: PathBuf,
    /// Root of the private directories for the QMP/VNC sockets: it must be short
    /// (107-byte limit of a Unix socket path).
    pub private_root: PathBuf,
    /// Nominal CPU model (F0: `qemu64`; F1-17 evaluates richer models).
    pub cpu_model: String,
    /// Ordinary stop policy (`system_powerdown` → `quit` → kill).
    pub stop_policy: ShutdownPolicy,
    /// Maximum wait for the Emergency Stop and the forced stop.
    pub emergency_timeout: Duration,
    /// Run configuration.
    pub run_config: RunConfig,
    /// Probe configuration (hashes of the approved binaries, timeout).
    pub probe: ProbeConfig,
    /// Free space for the catalog in GiB, used only if the measurement on the
    /// catalog volume (`prepare::host_resources`) fails. It limits `disk_gib`
    /// at creation and disk growth; a disk already created stays bootable even
    /// with less free space.
    pub free_storage_gib: u64,
}

impl QemuSetup {
    /// Default values for a distribution and a catalog.
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

/// Total physical memory in MiB, if detectable.
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

/// Logical processors of the host.
fn logical_cpus() -> u32 {
    std::thread::available_parallelism()
        .map(|n| u32::try_from(n.get()).unwrap_or(u32::MAX))
        .unwrap_or(1)
}

/// Job Object limits from the VM limits (ADR-0017):
/// - CPU cap: `cpu_max_percent` of the capacity of the assigned vCPUs,
///   expressed as a share of the whole machine in hundredths of a percentage
///   point (`vcpus × percent × 100 / logical processors`); 100 % = no cap;
/// - job memory: guest RAM plus `memory_margin_mib`.
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

/// Bytes used by the files under `dir` (recursive, without following links),
/// excluding `excluded`.
fn dir_usage(dir: &Path, excluded: &Path) -> u64 {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return 0;
    };
    rd.filter_map(Result::ok)
        .map(|e| {
            let p = e.path();
            match std::fs::symlink_metadata(&p) {
                Ok(m) if m.is_dir() => dir_usage(&p, excluded),
                Ok(m) if m.is_file() && p != excluded => m.len(),
                _ => 0,
            }
        })
        .sum()
}

/// Host capacity for the core, derived from the probe.
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
        // Not connected to the service in F1 (see the module header).
        screenshot_available: false,
        input_available: false,
        baseline_available: false,
    }
}

/// Probe of the approved binaries and of the accelerator; returns the host,
/// the VM backend and the host capacity for the core.
///
/// Must be called outside a tokio runtime (it uses `block_on` on the backend's
/// own runtime).
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
        .ok_or_else(|| PublicError::unsupported("no versioned q35 machine in the binary"))?;
    let cpu = caps.cpu_model(&setup.cpu_model).map_err(|e| public(&e))?;
    let probe_dir = setup.private_root.join("probe");
    std::fs::create_dir_all(&probe_dir).map_err(|e| io_public("probe directory", &e))?;
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
            "lightvm-service: accelerator not available: {}",
            caps.accelerator.public_message()
        );
    }
    let free_storage_gib = ManagedPath::verify(setup.catalog_root.clone())
        .and_then(|root| prepare::host_resources(&root))
        .map(|r| r.free_disk_bytes >> 30)
        .unwrap_or_else(|e| {
            crate::diag!(
                "lightvm-service: catalog free space not measured ({e}): using {} GiB",
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
        private_dirs: BTreeMap::new(),
        registry,
        stop_override,
    };
    Ok((host, backend, capacity))
}

/// Paths of a run for `qemu-backend`, all decided by the service.
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
            Err(BackendError::invalid("run_id", "different run"))
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
            .ok_or_else(|| BackendError::invalid("media", "medium not verified by the core"))
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

/// Absolute path of a managed copy (`ManagedRelPath`, `/` separator).
fn managed_abs(root: &Path, rel: &str) -> PathBuf {
    rel.split('/').fold(root.to_path_buf(), |p, c| p.join(c))
}

/// VM backend over `qemu-backend`.
pub struct QemuVmBackend {
    rt: tokio::runtime::Runtime,
    caps: HostCapabilities,
    machine: MachineType,
    cpu: CpuModel,
    setup: QemuSetup,
    runs: BTreeMap<VmId, QemuRun>,
    /// Private (socket) directory of each run, removed on release.
    private_dirs: BTreeMap<VmId, PathBuf>,
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
                "run differs from the active one",
            )),
            None => Err(PublicError::state_conflict("no active run")),
        }
    }

    /// Releases a terminated run: QMP, sockets, protected files, Job Object,
    /// private directory.
    fn release(&mut self, vm_id: VmId) {
        lock(&self.registry).remove(&vm_id);
        if let Some(run) = self.runs.remove(&vm_id) {
            match self.rt.block_on(run.close()) {
                Ok(r) if r.active_at_close > 0 => crate::diag!(
                    "lightvm-service: {} processes still active when closing the run",
                    r.active_at_close
                ),
                Ok(_) => {}
                Err(e) => crate::diag!("lightvm-service: closing the run: {e}"),
            }
        }
        if let Some(dir) = self.private_dirs.remove(&vm_id) {
            remove_private_dir(&dir);
        }
    }

    /// Run already exited (also through the final termination of the stop
    /// escalation, recorded as an Emergency Stop by the backend): it can be
    /// released without waiting any further.
    fn exited(&self, vm_id: VmId) -> bool {
        self.runs
            .get(&vm_id)
            .is_some_and(|r| matches!(r.state(), ProcessState::Exited { .. }))
    }

    /// Primary disk of the VM (`vms/<vm_id>/primary.qcow2`, `vm-storage`
    /// layout): created with `qemu-img` on first start with the size of
    /// `disk_gib`, then reused. Creation does not go through the catalog
    /// journal yet (F3): a pre-existing file is used as is.
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
            .ok_or_else(|| PublicError::resource_limit("disk out of range"))?;
        if std::fs::symlink_metadata(&path).is_ok() {
            // Existing disk: brought to the configured size if it was increased
            // in the VM settings (never shrunk).
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
                    "lightvm-service: VM disk grown to {} GiB",
                    vm.config.disk_gib
                );
            }
            return Ok(disk);
        }
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| io_public("VM directory", &e))?;
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
            PublicError::unsupported("UEFI firmware not found in the QEMU distribution")
        })?;
        let run_text = run_id.to_string();
        let run_dir = self.setup.catalog_root.join("runs").join(&run_text);
        std::fs::create_dir_all(&run_dir).map_err(|e| io_public("run directory", &e))?;
        // Last 12 hexadecimal characters (random part of the UUID v7): short path.
        let short: String = run_text
            .chars()
            .filter(char::is_ascii_hexdigit)
            .rev()
            .take(12)
            .collect();
        let private_dir = self.setup.private_root.join(short);
        std::fs::create_dir_all(&private_dir).map_err(|e| io_public("private directory", &e))?;
        let vars = run_dir.join("vars.fd");
        std::fs::copy(&fw.vars_template, &vars).map_err(|e| io_public("UEFI variables", &e))?;
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
        if self.exited(vm.id) {
            // Previous run exited but never released (for example a stop that
            // completed after the escalation timeout).
            self.release(vm.id);
        }
        if self.runs.contains_key(&vm.id) {
            return Err(PublicError::state_conflict("run already active for the VM"));
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
                return Err(PublicError::resource_limit("VM storage budget exceeded")
                    .with_detail("used_mib", (used >> 20).to_string())
                    .with_detail("budget_gib", budget.to_string()));
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
                .map_err(|_| PublicError::resource_limit("memory out of range"))?,
            disk: Some(DiskSpec {
                format: DiskFormat::Qcow2,
            }),
            install_media: vm.config.install_media,
            data_media: vm.config.data_media,
            limits: job_limits(&vm.config, logical_cpus()),
        };
        let private_dir = paths.private_dir.as_path().to_path_buf();
        let run = match self.rt.block_on(QemuRun::start(
            &self.caps,
            &spec,
            &paths,
            self.setup.run_config.clone(),
        )) {
            Ok(run) => run,
            Err(e) => {
                remove_private_dir(&private_dir);
                return Err(public(&e));
            }
        };
        lock(&self.registry).insert(vm.id, run.emergency_stop_handle());
        self.runs.insert(vm.id, run);
        self.private_dirs.insert(vm.id, private_dir);
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
            // The process exited even though the escalation ran out of time:
            // the stop is complete anyway.
            Err(_) if self.exited(vm_id) => {
                self.release(vm_id);
                Ok(())
            }
            Err(e) => Err(public(&e)),
        }
    }

    fn force_stop(&mut self, vm_id: VmId, run_id: RunId) -> Result<(), PublicError> {
        if !self.runs.contains_key(&vm_id) {
            // No owned process: nothing to terminate.
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
            "screenshot not yet connected to the service",
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
            "input not yet connected to the service",
        ))
    }
}

/// Removes the private directory of a run (sockets already closed).
fn remove_private_dir(dir: &Path) {
    if let Err(e) = std::fs::remove_dir_all(dir)
        && e.kind() != std::io::ErrorKind::NotFound
    {
        crate::diag!(
            "lightvm-service: private directory not removed ({}): {e}",
            dir.display()
        );
    }
}

impl BackendPort for QemuVmBackend {
    fn drain_observations(&mut self) -> Vec<(VmId, BackendObservation)> {
        let exited: Vec<VmId> = self
            .runs
            .iter()
            .filter(|(_, r)| match r.state() {
                // Exits from an Emergency Stop are confirmed by the core with
                // `force_stop` after the priority path.
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
                        "lightvm-service: QEMU terminated at the VM memory limit (peak {} MiB, limit {} MiB)",
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

/// Service host with the QEMU backend.
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
        // Copy of the handlers: the registry does not stay locked during the wait.
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
        // No agent sessions in F1: non-UI connections and credentials are
        // already revoked by the service; the core sessions arrive with F4.
    }
    fn prepare_shutdown(&mut self) {
        // ACPI 15 s → `quit` 5 s → termination 5 s per VM: within the 30 s per
        // VM of the service shutdown budget. A guest that powers off earlier
        // ends earlier; a `shutdown_vms` policy already set stays.
        let mut o = lock(&self.stop_override);
        if o.is_none() {
            *o = Some(ShutdownPolicy {
                powerdown_timeout: Duration::from_secs(15),
                quit_timeout: Duration::from_secs(5),
                kill_timeout: Duration::from_secs(5),
            });
        }
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
        // The engine has already stopped the VMs; only unconfirmed runs remain,
        // terminated through the priority path.
        let leftover = QemuEmergency {
            registry: self.registry.clone(),
            timeout: self.emergency_timeout,
        }
        .emergency_stop_all();
        if leftover.processes_terminated > 0 {
            crate::diag!(
                "lightvm-service: {} runs terminated at shutdown",
                leftover.processes_terminated
            );
        }
    }
}

/// VM backend without a configured QEMU: every start is rejected.
#[derive(Debug, Default)]
pub struct UnavailableBackend;

fn not_configured() -> PublicError {
    PublicError::unsupported("QEMU backend not configured in this service")
}

impl VmBackend for UnavailableBackend {
    fn start(&mut self, _: &VmRecord, _: RunId, _: &[VerifiedMedia]) -> Result<(), PublicError> {
        Err(not_configured())
    }
    fn shutdown(&mut self, _: VmId, _: RunId) -> Result<(), PublicError> {
        Err(not_configured())
    }
    fn force_stop(&mut self, _: VmId, _: RunId) -> Result<(), PublicError> {
        // No process can exist without a backend.
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

/// Host without QEMU: no process to terminate.
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
        "not configured".into()
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

/// Host capacity without QEMU: no approved accelerator, so `vm.start` is not
/// in the effective policy (`unsupported` error, no TCG fallback).
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
    fn job_limits_from_vm_limits() {
        let mut c = sample_config("limits");
        c.vcpus = 2;
        c.memory_mib = 2048;
        // Defaults: no CPU cap, memory = RAM + 1024 MiB.
        let l = job_limits(&c, 32);
        assert_eq!(l.cpu_rate, None);
        assert_eq!(l.job_memory_bytes, Some(3072 << 20));
        // 50 % of 2 vCPUs on 32 logical processors = 1 core = 3.12 % of the machine.
        c.limits = VmLimits {
            cpu_max_percent: 50,
            memory_margin_mib: 512,
            storage_budget_gib: None,
        };
        let l = job_limits(&c, 32);
        assert_eq!(l.cpu_rate, Some(312));
        assert_eq!(l.job_memory_bytes, Some(2560 << 20));
        // Never below 1 nor above 100 % of the machine.
        c.vcpus = 1;
        c.limits.cpu_max_percent = 5;
        assert_eq!(job_limits(&c, 1024).cpu_rate, Some(1));
        c.vcpus = 64;
        c.limits.cpu_max_percent = 99;
        assert_eq!(job_limits(&c, 4).cpu_rate, Some(10_000));
    }

    #[test]
    fn directory_usage_excludes_the_primary_disk() {
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

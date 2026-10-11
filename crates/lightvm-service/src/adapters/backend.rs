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
//! - the run directory is created here; the private socket directory (QMP,
//!   VNC, screendumps) gets the explicit user-only ACL
//!   (`qemu_backend::prepare::prepare_run_dirs`, F2); the private directory
//!   is removed when the run is released, the run directory (`runs/<run_id>/`,
//!   QEMU log) stays for diagnostics until the VM is deleted and does not
//!   count towards the VM storage budget (it counts in `disk_used_bytes`);
//! - the UEFI variables are per VM (`vms/<vm_id>/firmware/primary.vars.fd`),
//!   copied from the firmware template of the QEMU distribution on the first
//!   start and reused afterwards ([`ensure_vm_vars`], F2);
//! - the graceful stop runs in the background on the backend runtime
//!   ([`BackendPort::begin_shutdown`]): the supervisor thread is free during
//!   the ACPI wait and a forced stop can overtake it (F2); a stop whose
//!   escalation times out without observing the exit leaves the run
//!   **awaiting exit**: it is not released and its exit is reported by
//!   [`BackendPort::drain_observations`] whatever the exit cause (the
//!   escalation's own termination included);
//! - exits caused by the **priority path** (Emergency Stop) are told apart
//!   from the escalation's termination: the priority handler marks the run
//!   before terminating it, and such runs are left to the core's
//!   `after_emergency_stop` (`force_stop` confirms the end), never reported
//!   as an ordinary stop or exit, even when a graceful stop was pending;
//! - the private directory of a run is removed on every failed start path
//!   ([`PrivateDirGuard`]);
//! - screenshot (`screendump`, metadata only over IPC), input
//!   (`input-send-event`), reset and the display socket of the run (for the
//!   console proxy, `crate::display`) are connected (F2);
//! - F3: a VM restored from a baseline starts on its current overlay
//!   ([`BackendPort::set_start_disks`]): overlay and UEFI variables of the
//!   current state, the baseline's base disk as an explicit read-only
//!   backing node, base and baseline variables protected read-only for the
//!   whole run, the catalog guard kept until the run is released; the
//!   standalone disk is neither created nor used. `qemu-img` operations for
//!   baselines ([`QemuImageTool`]) run in their own Job Object, cancellable.
//!   The end of every run is reported with its cause
//!   ([`BackendPort::take_exits`]: clean only for an observed guest shutdown).
//! - the handles through which the approved media were hashed for a start
//!   ([`BackendPort::set_start_media`], `crate::start`) are kept with the
//!   catalog guard until the run is released: while the run lives the media
//!   copies cannot be written, renamed or deleted, so QEMU reads the bytes
//!   that were verified.

use std::collections::{BTreeMap, BTreeSet};
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
    image, prepare,
};
use vm_core::media::VerifiedMedia;
use vm_core::ports::VmBackend;
use vm_storage::CatalogLayout;

use crate::engine::{BackendPort, GuardedMedia, ImageTool, StartDisks};
use crate::supervisor::{Backend, EmergencyOutcome, EmergencyStop, ParentLossPolicy};

type Registry = Arc<Mutex<BTreeMap<VmId, (RunId, qemu_backend::EmergencyStop)>>>;

/// Runs terminated by the priority path (Emergency Stop), marked before the
/// termination: their exit belongs to the core's `after_emergency_stop`.
type PriorityKilled = Arc<Mutex<BTreeSet<(VmId, RunId)>>>;

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
    /// Named CPU model. Default `Skylake-Client-v4`: with `qemu64` the
    /// reference guest (Ubuntu 26.04) does not reach the desktop (deviation
    /// D-1); `Skylake-Client-v4` is the model verified for live boot,
    /// installation and boot from disk (F1-17, F2-01). `-cpu host` stays
    /// refused (ADR-0011). Hosts whose CPU lacks the model's features are not
    /// verified (single reference host).
    pub cpu_model: String,
    /// Ordinary stop policy (`system_powerdown` â†’ `quit` â†’ kill).
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
            cpu_model: "Skylake-Client-v4".into(),
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
///   point (`vcpus Ã— percent Ã— 100 / logical processors`); 100 % = no cap;
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
        // `screendump` and `input-send-event` through QMP (F2).
        screenshot_available: true,
        input_available: true,
        // `qemu-img` image operations and overlays (F3).
        baseline_available: true,
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
    // Private directory for the outputs of the `qemu-img` commands (F3).
    let img_dir = setup.private_root.join("img");
    std::fs::create_dir_all(&img_dir).map_err(|e| io_public("image log directory", &e))?;
    // Outputs left by `qemu-img` commands of a previous service that died
    // (prudent cleanup: only our own file names, regular files).
    if let Ok(rd) = std::fs::read_dir(&img_dir) {
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            let ours =
                name.starts_with("img-") && (name.ends_with(".out") || name.ends_with(".err"));
            if ours && e.file_type().is_ok_and(|t| t.is_file()) {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }
    let img_dir = ManagedPath::verify(img_dir).map_err(|e| public(&e))?;
    if cfg!(windows) {
        prepare::restrict_to_current_user(&img_dir).map_err(|e| public(&e))?;
    }
    let image_tool = Arc::new(QemuImageTool {
        qemu_img: caps.qemu_img.clone(),
        log_dir: img_dir,
        vars_template: caps.firmware.as_ref().map(|f| f.vars_template.clone()),
    });
    let registry: Registry = Arc::default();
    let priority_killed: PriorityKilled = Arc::default();
    let stop_override: StopOverride = Arc::default();
    let host = QemuHost {
        version: caps.version.to_string(),
        accelerator: caps
            .accelerator
            .is_available()
            .then(|| Accelerator::for_current_host().qemu_name().to_owned()),
        registry: registry.clone(),
        priority_killed: priority_killed.clone(),
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
        stops: BTreeMap::new(),
        private_dirs: BTreeMap::new(),
        registry,
        priority_killed,
        stop_override,
        start_disks: BTreeMap::new(),
        start_media: BTreeMap::new(),
        guards: BTreeMap::new(),
        exits: Vec::new(),
        image_tool,
        last_shots: BTreeMap::new(),
    };
    Ok((host, backend, capacity))
}

/// `qemu-img` operations for baselines (F3), each in its own Job Object
/// (`qemu_backend::image`). Paths are catalog paths decided by the storage.
#[derive(Debug)]
pub struct QemuImageTool {
    qemu_img: ManagedPath,
    log_dir: ManagedPath,
    vars_template: Option<PathBuf>,
}

impl QemuImageTool {
    fn cmd(&self, long: bool) -> image::ImageCommand<'_> {
        image::ImageCommand {
            qemu_img: &self.qemu_img,
            log_dir: &self.log_dir,
            timeout: if long {
                image::LONG_IMAGE_TIMEOUT
            } else {
                image::SHORT_IMAGE_TIMEOUT
            },
        }
    }
}

fn image_error(e: &BackendError) -> PublicError {
    match e {
        BackendError::StateConflict(m) if m.contains("cancelled") => {
            PublicError::state_conflict("operation cancelled")
        }
        other => public(other),
    }
}

fn verified(p: &Path) -> Result<ManagedPath, PublicError> {
    ManagedPath::verify(p).map_err(|e| public(&e))
}

impl ImageTool for QemuImageTool {
    fn convert(
        &self,
        src: &Path,
        dst: &Path,
        cancel: &dyn Fn() -> bool,
    ) -> Result<(), PublicError> {
        image::convert_standalone(&self.cmd(true), &verified(src)?, &verified(dst)?, cancel)
            .map_err(|e| image_error(&e))
    }
    fn compare(&self, a: &Path, b: &Path, cancel: &dyn Fn() -> bool) -> Result<bool, PublicError> {
        image::images_identical(&self.cmd(true), &verified(a)?, &verified(b)?, cancel)
            .map_err(|e| image_error(&e))
    }
    fn check(&self, p: &Path, cancel: &dyn Fn() -> bool) -> Result<(), PublicError> {
        image::check_image(&self.cmd(true), &verified(p)?, cancel).map_err(|e| image_error(&e))
    }
    fn virtual_size(&self, p: &Path) -> Result<u64, PublicError> {
        image::virtual_size(&self.cmd(false), &verified(p)?).map_err(|e| image_error(&e))
    }
    fn create_overlay(&self, backing_ref: &str, size: u64, dst: &Path) -> Result<(), PublicError> {
        image::create_overlay_ref(&self.cmd(false), backing_ref, size, &verified(dst)?)
            .map_err(|e| image_error(&e))
    }
    fn firmware_vars_template(&self) -> Option<PathBuf> {
        self.vars_template.clone()
    }
}

/// Paths of a run for `qemu-backend`, all decided by the service.
#[derive(Debug)]
struct RunPaths {
    run_id: RunId,
    qemu_system: ManagedPath,
    firmware_code: ManagedPath,
    firmware_vars: ManagedPath,
    disk: ManagedPath,
    /// Base of the overlay (F3), explicit backing node.
    backing: Option<ManagedPath>,
    /// Base and baseline variables, protected read-only (F3).
    readonly: Vec<ManagedPath>,
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
        Ok(self.readonly.clone())
    }
    fn disk_backing(&self, run: &RunId) -> qemu_backend::error::Result<Option<ManagedPath>> {
        self.check(run)?;
        Ok(self.backing.clone())
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

/// Graceful stop of a run in progress on the backend runtime.
type StopTask = (
    RunId,
    tokio::task::JoinHandle<qemu_backend::error::Result<qemu_backend::ExitReport>>,
);

/// VM backend over `qemu-backend`.
pub struct QemuVmBackend {
    rt: tokio::runtime::Runtime,
    caps: HostCapabilities,
    machine: MachineType,
    cpu: CpuModel,
    setup: QemuSetup,
    runs: BTreeMap<VmId, Arc<QemuRun>>,
    /// Graceful stops running in the background (escalation of
    /// `QemuRun::shutdown`), per VM.
    stops: BTreeMap<VmId, StopTask>,
    /// Private (socket) directory of each run, removed on release.
    private_dirs: BTreeMap<VmId, PathBuf>,
    registry: Registry,
    priority_killed: PriorityKilled,
    stop_override: StopOverride,
    /// Current-state files for the next start of a VM (F3), by run.
    start_disks: BTreeMap<VmId, (RunId, StartDisks)>,
    /// Protected handles of the media verified for the next start of a VM.
    start_media: BTreeMap<VmId, (RunId, Vec<Box<dyn GuardedMedia>>)>,
    /// Guards of the running VMs (catalog guard of the current state, F3;
    /// handles of the verified media), dropped on release.
    guards: BTreeMap<VmId, Vec<Box<dyn Send>>>,
    /// Ends of runs not yet reported (`true` = observed guest shutdown).
    exits: Vec<(VmId, RunId, bool)>,
    /// Image operations for baselines.
    image_tool: Arc<QemuImageTool>,
    /// Last screenshot of each VM (PNG in memory, at most the byte limit of
    /// the request), taken once by the engine for an agent session (F4).
    last_shots: BTreeMap<VmId, qemu_backend::screenshot::CapturedScreenshot>,
}

/// Route of a graceful stop task that ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FinishedStop {
    /// The run was terminated by the priority path: not reported, the run is
    /// kept for the core's `after_emergency_stop` (`force_stop`).
    LeaveToEmergency,
    /// Exit observed: run released, stop reported as completed.
    Released,
    /// Escalation timed out with the process still alive: stop reported as
    /// failed, run kept "awaiting exit" (its exit is reported later by
    /// `drain_observations`, whatever the cause).
    AwaitingExit,
}

fn finished_stop_route(priority_killed: bool, stop_ok: bool, exited: bool) -> FinishedStop {
    if priority_killed {
        FinishedStop::LeaveToEmergency
    } else if stop_ok || exited {
        FinishedStop::Released
    } else {
        FinishedStop::AwaitingExit
    }
}

/// `true` if the exit of a run is to be reported as an observation: the
/// process exited, no graceful stop is in progress for it (that one reports
/// through `finished_shutdowns`) and the priority path did not terminate it.
/// Any other exit cause is reported, the escalation's termination included.
fn exit_to_report(state: &ProcessState, graceful_in_progress: bool, priority_killed: bool) -> bool {
    !graceful_in_progress && !priority_killed && matches!(state, ProcessState::Exited { .. })
}

/// Removes a run's private directory when dropped, unless disarmed: every
/// error path of a start (preparation, firmware, disk, launch) cleans it up.
struct PrivateDirGuard(Option<PathBuf>);

impl PrivateDirGuard {
    fn new(dir: PathBuf) -> Self {
        Self(Some(dir))
    }

    /// The run started: the directory now belongs to the run.
    fn disarm(mut self) -> PathBuf {
        self.0.take().unwrap_or_default()
    }
}

impl Drop for PrivateDirGuard {
    fn drop(&mut self) {
        if let Some(dir) = self.0.take() {
            remove_private_dir(&dir);
        }
    }
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
    fn run(&self, vm_id: VmId, run_id: RunId) -> Result<&Arc<QemuRun>, PublicError> {
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
        self.last_shots.remove(&vm_id);
        lock(&self.priority_killed).retain(|(vm, _)| *vm != vm_id);
        // A graceful stop still running ends as soon as it sees the exit
        // (the process is gone when a run is released).
        if let Some((_, task)) = self.stops.remove(&vm_id) {
            let _ = self.rt.block_on(task);
        }
        if let Some(run) = self.runs.get(&vm_id) {
            // F3: only an observed guest shutdown is a clean end.
            let clean = matches!(
                run.state(),
                ProcessState::Exited {
                    cause: ExitCause::GuestShutdown,
                    ..
                }
            );
            self.exits.push((vm_id, run.run_id(), clean));
        }
        if let Some(run) = self.runs.remove(&vm_id) {
            let Ok(run) = Arc::try_unwrap(run) else {
                crate::diag!("lightvm-service: run still referenced, not closed");
                return;
            };
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
        // Catalog guard released after QEMU has closed the files.
        self.guards.remove(&vm_id);
    }

    /// The priority path (Emergency Stop) terminated this run.
    fn priority_killed(&self, vm_id: VmId, run_id: RunId) -> bool {
        lock(&self.priority_killed).contains(&(vm_id, run_id))
    }

    /// Private directory of a run: last 12 hexadecimal characters of the run
    /// (random part of the UUID v7), for a short socket path.
    fn private_dir_for(&self, run_id: RunId) -> PathBuf {
        let short: String = run_id
            .to_string()
            .chars()
            .filter(char::is_ascii_hexdigit)
            .rev()
            .take(12)
            .collect();
        self.setup.private_root.join(short)
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
            self.grow(vm, &disk)?;
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

    /// Existing disk (standalone or overlay): brought to the configured size
    /// if it was increased in the VM settings (never shrunk).
    fn grow(&self, vm: &VmRecord, disk: &ManagedPath) -> Result<(), PublicError> {
        let size_mib = vm
            .config
            .disk_gib
            .checked_mul(1024)
            .ok_or_else(|| PublicError::resource_limit("disk out of range"))?;
        if prepare::grow_disk(
            &self.caps.qemu_img,
            disk,
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
        Ok(())
    }

    fn prepare(
        &self,
        vm: &VmRecord,
        run_id: RunId,
        media: &[VerifiedMedia],
        current: Option<&StartDisks>,
    ) -> Result<RunPaths, PublicError> {
        let fw = self.caps.firmware.as_ref().ok_or_else(|| {
            PublicError::unsupported("UEFI firmware not found in the QEMU distribution")
        })?;
        let run_dir = self
            .setup
            .catalog_root
            .join("runs")
            .join(run_id.to_string());
        let private_dir = self.private_dir_for(run_id);
        let run_dir_m = ManagedPath::verify(run_dir.clone()).map_err(|e| public(&e))?;
        let private_m = ManagedPath::verify(private_dir.clone()).map_err(|e| public(&e))?;
        if cfg!(windows) {
            // Run directory plus private directory with the user-only ACL.
            prepare::prepare_run_dirs(&run_dir_m, &private_m).map_err(|e| public(&e))?;
        } else {
            std::fs::create_dir_all(&run_dir).map_err(|e| io_public("run directory", &e))?;
            std::fs::create_dir_all(&private_dir)
                .map_err(|e| io_public("private directory", &e))?;
        }
        let mut media_paths = BTreeMap::new();
        for m in media {
            let p = managed_abs(&self.setup.catalog_root, m.entry().managed_path.as_str());
            media_paths.insert(
                m.record().id,
                ManagedPath::verify(p).map_err(|e| public(&e))?,
            );
        }
        if let Some(d) = current {
            // F3: current state = overlay of a baseline, files verified and
            // guarded by the catalog; the standalone disk is not touched.
            let vars = d.vars.clone().ok_or_else(|| {
                PublicError::state_conflict("current state without UEFI variables")
            })?;
            let disk = ManagedPath::verify(d.overlay.clone()).map_err(|e| public(&e))?;
            self.grow(vm, &disk)?;
            let mut readonly = vec![ManagedPath::verify(d.base.clone()).map_err(|e| public(&e))?];
            if let Some(bv) = &d.base_vars {
                readonly.push(ManagedPath::verify(bv.clone()).map_err(|e| public(&e))?);
            }
            return Ok(RunPaths {
                run_id,
                qemu_system: self.caps.qemu_system.clone(),
                firmware_code: ManagedPath::verify(fw.code.clone()).map_err(|e| public(&e))?,
                firmware_vars: ManagedPath::verify(vars).map_err(|e| public(&e))?,
                disk,
                backing: Some(readonly[0].clone()),
                readonly,
                run_dir: ManagedPath::verify(run_dir).map_err(|e| public(&e))?,
                private_dir: ManagedPath::verify(private_dir).map_err(|e| public(&e))?,
                media: media_paths,
            });
        }
        // Firmware state of the VM: created from the template on the first
        // start, then reused (boot entries written by an installer persist).
        let vars = managed_abs(
            &self.setup.catalog_root,
            CatalogLayout::primary_vars(&vm.id).as_str(),
        );
        if ensure_vm_vars(&fw.vars_template, &vars)
            .map_err(|e| io_public("UEFI variables of the VM", &e))?
        {
            crate::diag!("lightvm-service: UEFI variables of the VM created from the template");
        }
        let disk = self.primary_disk(vm)?;
        Ok(RunPaths {
            run_id,
            qemu_system: self.caps.qemu_system.clone(),
            firmware_code: ManagedPath::verify(fw.code.clone()).map_err(|e| public(&e))?,
            firmware_vars: ManagedPath::verify(vars).map_err(|e| public(&e))?,
            disk,
            backing: None,
            readonly: Vec::new(),
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
        // F3: files of the current state for this run, if the catalog gave
        // them (with its guard, kept until the run is released).
        let current = self
            .start_disks
            .remove(&vm.id)
            .filter(|(r, _)| *r == run_id)
            .map(|(_, d)| d);
        // Handles of the media verified for this run: kept from before the
        // hash until the run is released (dropped here if the start fails).
        let media_guards = self
            .start_media
            .remove(&vm.id)
            .filter(|(r, _)| *r == run_id)
            .map(|(_, g)| g)
            .unwrap_or_default();
        // From here on every error path removes the private directory.
        let private_guard = PrivateDirGuard::new(self.private_dir_for(run_id));
        let paths = self.prepare(vm, run_id, media, current.as_ref())?;
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
        let run = self
            .rt
            .block_on(QemuRun::start(
                &self.caps,
                &spec,
                &paths,
                self.setup.run_config.clone(),
            ))
            .map_err(|e| public(&e))?;
        lock(&self.registry).insert(vm.id, (run_id, run.emergency_stop_handle()));
        self.runs.insert(vm.id, Arc::new(run));
        self.private_dirs.insert(vm.id, private_guard.disarm());
        let mut held: Vec<Box<dyn Send>> = Vec::new();
        if let Some(d) = current {
            held.push(Box::new(d.guard));
        }
        held.extend(
            media_guards
                .into_iter()
                .map(|g| Box::new(g) as Box<dyn Send>),
        );
        self.guards.insert(vm.id, held);
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

    /// `screendump` in the private directory; the metadata (geometry, hash,
    /// size) goes to the core, the PNG stays here until the engine takes it
    /// for an agent session ([`BackendPort::take_screenshot`], F4; the
    /// console uses RFB).
    fn screenshot(
        &mut self,
        vm_id: VmId,
        run_id: RunId,
        max_bytes: u64,
    ) -> Result<Screenshot, PublicError> {
        let run = self.run(vm_id, run_id)?;
        let shot = self
            .rt
            .block_on(run.screenshot_limited(max_bytes))
            .map_err(|e| public(&e))?;
        let meta = shot.metadata(vm_id);
        self.last_shots.insert(vm_id, shot);
        Ok(meta)
    }

    /// `input-send-event`. A batch without pointer movements does not depend
    /// on the geometry: it is sent with the run's current one, so a keyboard
    /// batch is never refused for a resolution change.
    fn send_input(
        &mut self,
        vm_id: VmId,
        run_id: RunId,
        geometry: DisplayGeometry,
        events: &[InputEvent],
    ) -> Result<(), PublicError> {
        let run = self.run(vm_id, run_id)?;
        let pointer = events
            .iter()
            .any(|e| matches!(e, InputEvent::PointerMove(_)));
        let g = if pointer {
            geometry
        } else {
            run.current_geometry().unwrap_or(geometry)
        };
        self.rt
            .block_on(run.send_input(run_id, g, events))
            .map_err(|e| public(&e))
    }
}

/// Persistent UEFI variables of a VM (`vms/<vm_id>/firmware/primary.vars.fd`,
/// `vm-storage` layout): if missing, copied from the firmware template into
/// a temporary file next to it, synced and published with a hard link that
/// never replaces an existing file; if present (regular file, not a link) it
/// is reused as is. Returns `true` if it was created. Like the primary disk
/// it is made by the backend, not through the catalog journal (F3); it is
/// protected during the run by the storage guard (`firmware_vars` is in the
/// no-delete set of `QemuRun::start`), counted in `disk_used_bytes` and
/// deleted with the VM (`vms/<vm_id>/`).
pub fn ensure_vm_vars(template: &Path, dest: &Path) -> std::io::Result<bool> {
    match std::fs::symlink_metadata(dest) {
        Ok(m) if m.is_file() && !crate::adapters::catalog::is_link(&m) => return Ok(false),
        Ok(_) => {
            return Err(std::io::Error::other(
                "UEFI variables of the VM: not a regular file",
            ));
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    if let Some(dir) = dest.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = dest.with_extension("fd.part");
    let _ = std::fs::remove_file(&tmp);
    let result = (|| {
        std::fs::copy(template, &tmp)?;
        std::fs::OpenOptions::new()
            .write(true)
            .open(&tmp)?
            .sync_all()?;
        std::fs::hard_link(&tmp, dest)
    })();
    let _ = std::fs::remove_file(&tmp);
    result.map(|()| true)
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
    fn image_tool(&self) -> Option<Arc<dyn ImageTool>> {
        Some(self.image_tool.clone())
    }

    fn set_start_disks(&mut self, vm_id: VmId, run_id: RunId, disks: StartDisks) {
        self.start_disks.insert(vm_id, (run_id, disks));
    }

    fn set_start_media(&mut self, vm_id: VmId, run_id: RunId, guards: Vec<Box<dyn GuardedMedia>>) {
        self.start_media.insert(vm_id, (run_id, guards));
    }

    fn take_exits(&mut self) -> Vec<(VmId, RunId, bool)> {
        std::mem::take(&mut self.exits)
    }

    fn display_socket(&self, vm_id: VmId, run_id: RunId) -> Option<PathBuf> {
        self.run(vm_id, run_id)
            .ok()
            .and_then(|r| r.display_socket())
            .map(Path::to_path_buf)
    }

    /// `system_powerdown` and escalation (`quit`, termination) on the
    /// backend runtime: the supervisor thread is not held for the ACPI wait.
    fn begin_shutdown(&mut self, vm_id: VmId, run_id: RunId) -> Option<Result<(), PublicError>> {
        let run = match self.run(vm_id, run_id) {
            Ok(r) => r.clone(),
            Err(e) => return Some(Err(e)),
        };
        if self.stops.contains_key(&vm_id) {
            return Some(Err(PublicError::state_conflict("stop already in progress")));
        }
        let policy = lock(&self.stop_override).unwrap_or(self.setup.stop_policy);
        let task = self.rt.spawn(async move { run.shutdown(policy).await });
        self.stops.insert(vm_id, (run_id, task));
        Some(Ok(()))
    }

    fn finished_shutdowns(&mut self) -> Vec<(VmId, RunId, Result<(), PublicError>)> {
        let done: Vec<VmId> = self
            .stops
            .iter()
            .filter(|(_, (_, t))| t.is_finished())
            .map(|(vm, _)| *vm)
            .collect();
        let mut out = Vec::new();
        for vm_id in done {
            let Some((run_id, task)) = self.stops.remove(&vm_id) else {
                continue;
            };
            let joined = self.rt.block_on(task);
            let stop_ok = matches!(joined, Ok(Ok(_)));
            match finished_stop_route(
                self.priority_killed(vm_id, run_id),
                stop_ok,
                self.exited(vm_id),
            ) {
                FinishedStop::LeaveToEmergency => {
                    crate::diag!("lightvm-service: graceful stop overtaken by the Emergency Stop");
                    continue;
                }
                FinishedStop::Released | FinishedStop::AwaitingExit => {}
            }
            let r = match joined {
                Ok(Ok(report)) => {
                    crate::diag!(
                        "lightvm-service: graceful stop: phase {:?}, cause {:?}, {:?}",
                        report.phase,
                        report.cause,
                        report.elapsed
                    );
                    Ok(())
                }
                // The process exited even though the escalation ran out of
                // time: the stop is complete anyway.
                Ok(Err(_)) if self.exited(vm_id) => Ok(()),
                Ok(Err(e)) => Err(public(&e)),
                Err(e) => Err(io_public(
                    "stop task",
                    &std::io::Error::other(e.to_string()),
                )),
            };
            if r.is_ok() {
                self.release(vm_id);
            } else {
                crate::diag!(
                    "lightvm-service: graceful stop without observed exit: run awaiting exit"
                );
            }
            out.push((vm_id, run_id, r));
        }
        out
    }

    fn refresh_geometry(&mut self, vm_id: VmId, run_id: RunId) -> Option<DisplayGeometry> {
        let run = self.run(vm_id, run_id).ok()?;
        match self.rt.block_on(run.refresh_geometry()) {
            Ok(g) => Some(g),
            Err(e) => {
                crate::diag!("lightvm-service: display geometry not read: {e}");
                None
            }
        }
    }

    fn take_screenshot(&mut self, vm_id: VmId, run_id: RunId) -> Option<(Screenshot, Vec<u8>)> {
        let shot = self.last_shots.remove(&vm_id)?;
        (shot.run_id == run_id).then(|| (shot.metadata(vm_id), shot.png))
    }

    fn release_input(&mut self, vm_id: VmId, run_id: RunId) {
        if let Ok(run) = self.run(vm_id, run_id)
            && let Err(e) = self.rt.block_on(run.release_all_input(run_id))
        {
            crate::diag!("lightvm-service: input release: {e}");
        }
    }

    fn drain_observations(&mut self) -> Vec<(VmId, BackendObservation)> {
        let exited: Vec<VmId> = self
            .runs
            .iter()
            .filter(|(vm, r)| {
                // A run with a graceful stop in progress is reported by
                // `finished_shutdowns`; one terminated by the priority path
                // is confirmed by the core with `force_stop` after it. Every
                // other exit is reported, also a run awaiting exit after a
                // stop whose escalation timed out.
                exit_to_report(
                    &r.state(),
                    self.stops.contains_key(vm),
                    self.priority_killed(**vm, r.run_id()),
                )
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
    priority_killed: PriorityKilled,
    emergency_timeout: Duration,
    stop_override: StopOverride,
}

#[derive(Debug)]
struct QemuEmergency {
    registry: Registry,
    priority_killed: PriorityKilled,
    timeout: Duration,
}

impl EmergencyStop for QemuEmergency {
    fn emergency_stop_all(&self) -> EmergencyOutcome {
        // Copy of the handlers: the registry does not stay locked during the wait.
        let handles: Vec<(VmId, RunId, qemu_backend::EmergencyStop)> = lock(&self.registry)
            .iter()
            .map(|(vm, (run, h))| (*vm, *run, h.clone()))
            .collect();
        // Marked before terminating: the exits that follow belong to the
        // Emergency Stop, never to a graceful stop or an ordinary exit.
        lock(&self.priority_killed).extend(handles.iter().map(|(vm, run, _)| (*vm, *run)));
        let mut out = EmergencyOutcome {
            processes_terminated: 0,
            completed: true,
        };
        for (_, _, h) in handles {
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
            priority_killed: self.priority_killed.clone(),
            timeout: self.emergency_timeout,
        })
    }
    fn stop_agent_sessions(&mut self, _reason: StopReason) {
        // No agent sessions in F1: non-UI connections and credentials are
        // already revoked by the service; the core sessions arrive with F4.
    }
    fn prepare_shutdown(&mut self) {
        // ACPI 15 s â†’ `quit` 5 s â†’ termination 5 s per VM: within the 30 s per
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
            priority_killed: self.priority_killed.clone(),
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
    fn vm_vars_are_created_once_and_then_reused() {
        let d = std::env::temp_dir().join(format!(
            "lightvm-vm-vars-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos())
        ));
        std::fs::create_dir_all(&d).unwrap();
        let template = d.join("template.fd");
        std::fs::write(&template, b"template").unwrap();
        let vars = d
            .join("vms")
            .join("x")
            .join("firmware")
            .join("primary.vars.fd");
        assert!(ensure_vm_vars(&template, &vars).unwrap());
        assert_eq!(std::fs::read(&vars).unwrap(), b"template");
        // The guest writes boot entries during the run.
        std::fs::write(&vars, b"written by the guest").unwrap();
        // Next start: reused, never overwritten by the template.
        assert!(!ensure_vm_vars(&template, &vars).unwrap());
        assert_eq!(std::fs::read(&vars).unwrap(), b"written by the guest");
        assert!(!vars.with_extension("fd.part").exists());
        // A directory in its place is refused.
        let bad = d.join("bad.fd");
        std::fs::create_dir_all(&bad).unwrap();
        assert!(ensure_vm_vars(&template, &bad).is_err());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn stop_and_exit_routing() {
        use qemu_backend::ExitCause;
        // Emergency Stop during a pending graceful stop: left to the core's
        // Emergency Stop recording, whatever the stop task returned.
        for (ok, exited) in [(true, true), (false, true), (false, false)] {
            assert_eq!(
                finished_stop_route(true, ok, exited),
                FinishedStop::LeaveToEmergency
            );
        }
        assert_eq!(
            finished_stop_route(false, true, true),
            FinishedStop::Released
        );
        assert_eq!(
            finished_stop_route(false, false, true),
            FinishedStop::Released
        );
        // Escalation timed out with the process alive: awaiting exit.
        assert_eq!(
            finished_stop_route(false, false, false),
            FinishedStop::AwaitingExit
        );
        let exited = |cause| ProcessState::Exited {
            code: Some(1),
            cause,
        };
        // The escalation's own termination is reported (awaiting exit).
        assert!(exit_to_report(
            &exited(ExitCause::EmergencyStop),
            false,
            false
        ));
        assert!(exit_to_report(
            &exited(ExitCause::GuestShutdown),
            false,
            false
        ));
        assert!(exit_to_report(&exited(ExitCause::Unexpected), false, false));
        // Priority path: left to `after_emergency_stop`.
        assert!(!exit_to_report(
            &exited(ExitCause::EmergencyStop),
            false,
            true
        ));
        // Graceful stop in progress: reported by `finished_shutdowns`.
        assert!(!exit_to_report(&exited(ExitCause::QmpQuit), true, false));
        assert!(!exit_to_report(&ProcessState::Running, false, false));
    }

    #[test]
    fn failed_start_steps_remove_the_private_dir() {
        let d = crate::test_support::temp_dir("private-guard");
        let private = d.join("abcdef012345");
        // A step after the preparation fails: the directory goes.
        std::fs::create_dir_all(&private).unwrap();
        std::fs::write(private.join("qmp.sock"), b"").unwrap();
        let start = |fail: bool| -> Result<PathBuf, PublicError> {
            let guard = PrivateDirGuard::new(private.clone());
            if fail {
                return Err(PublicError::infrastructure("QEMU backend: disk"));
            }
            Ok(guard.disarm())
        };
        assert!(start(true).is_err());
        assert!(!private.exists());
        // Successful start: the directory stays with the run.
        std::fs::create_dir_all(&private).unwrap();
        assert_eq!(start(false).unwrap(), private);
        assert!(private.exists());
        let _ = std::fs::remove_dir_all(&d);
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

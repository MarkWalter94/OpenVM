//! Running a VM: process, QMP, post-launch checks, state machine
//! and shutdown (ROADMAP 6.1, 6.3, 7.2, 7.3; ADR-0002, ADR-0007, ADR-0009).
//!
//! Sequence of [`QemuRun::start`]:
//!
//! 1. validation of the specification against the detected capabilities (accelerator
//!    verified by the probe and equal to the requested one, machine, CPU,
//!    devices);
//! 2. protective opening of the files ([`crate::storage_guard`]);
//! 3. removal of orphan sockets, spawn in the run's container (Job
//!    Object, or the watchdog process group on Unix): first swtpm for the
//!    TPM profiles (ADR-0028), waiting for its control socket, then QEMU
//!    with `-S`;
//! 4. QMP connection (sole client, socket in the private directory);
//! 5. verification of the active accelerator (`query-accelerators`) and of the absence
//!    of NICs (empty `info network` and no PCI device of the network class);
//!    on a negative outcome the process is terminated;
//! 6. `cont`: only now does the guest execute code.
//!
//! Process state: `Starting → Running → Stopping → Exited { code, cause }`,
//! observed by waiting on the process handle (dedicated thread) and from
//! QMP `SHUTDOWN` events. The [`EmergencyStop`] terminates the container with
//! OS primitives without going through the QMP queue. [`QemuRun::reconcile`] compares
//! process and QMP; [`QemuRun::close`] closes the Job Object after the exit.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use lightvm_proto::{
    BackendObservation, DisplayGeometry, InputEvent, ObservationSource, ObservedState, RunId,
    UtcTimestamp, VmState,
};
use tokio::sync::{broadcast, watch};

use crate::cmdline::{FirmwareKind, QemuCommand, TpmCommand, VmLaunchSpec};
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

/// Cause of the QEMU process exit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExitCause {
    /// The guest shut down (`SHUTDOWN` event with `guest: true`).
    GuestShutdown,
    /// Exit after `quit` requested by the backend.
    QmpQuit,
    /// Container termination requested by the backend.
    EmergencyStop,
    /// QEMU exited before completing startup.
    StartupFailed,
    /// Unrequested exit (crash, external termination).
    Unexpected,
}

/// State of the QEMU process of a run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProcessState {
    /// Process created, post-launch checks in progress.
    Starting,
    /// Checks passed, vCPUs started (or left paused on request).
    Running,
    /// Shutdown requested.
    Stopping,
    /// Process exited (observed on the handle).
    Exited {
        /// Exit code.
        code: Option<u32>,
        /// Cause.
        cause: ExitCause,
    },
}

/// Configuration of a run.
#[derive(Clone, Debug)]
pub struct RunConfig {
    /// QMP configuration.
    pub qmp: QmpConfig,
    /// Maximum time between the spawn and the QMP connection.
    pub startup_timeout: Duration,
    /// `screendump` timeout.
    pub screendump_timeout: Duration,
    /// Maximum size of a screenshot PNG.
    pub max_screenshot_bytes: u64,
    /// Leaves the vCPUs paused after the checks (no `cont`).
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

/// Graceful shutdown policy with escalation (F0-06, 2d): every timeout is
/// configurable.
#[derive(Clone, Copy, Debug)]
pub struct ShutdownPolicy {
    /// Wait after `system_powerdown` (zero = skip the powerdown).
    pub powerdown_timeout: Duration,
    /// Wait after `quit`.
    pub quit_timeout: Duration,
    /// Wait after the container termination.
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

/// Phase in which the shutdown completed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StopPhase {
    /// The process had already exited.
    AlreadyExited,
    /// Exit after `system_powerdown`.
    Powerdown,
    /// Exit after `quit`.
    Quit,
    /// Container termination.
    Kill,
}

/// Observed outcome of a shutdown.
#[derive(Clone, Debug)]
pub struct ExitReport {
    /// Exit code.
    pub code: Option<u32>,
    /// Class of the exit code.
    pub class: ExitCodeClass,
    /// Cause.
    pub cause: ExitCause,
    /// Final phase.
    pub phase: StopPhase,
    /// Time from the request to the observed exit.
    pub elapsed: Duration,
    /// Processes still active in the container after the exit (expected 0).
    pub active_after: u32,
    /// Escalation phases that failed (QMP command rejected or timed out,
    /// no exit within the timeout), without QEMU text.
    pub notes: Vec<String>,
}

/// Outcome of [`QemuRun::close`].
#[derive(Clone, Debug)]
pub struct CloseReport {
    /// Process identity of the closed run.
    pub identity: ProcessIdentity,
    /// `true` if the process was alive and was terminated with the Emergency Stop.
    pub forced: bool,
    /// Processes active in the container at close time (expected 0).
    pub active_at_close: u32,
    /// `true` if the Job Object handle was closed here; `false` if a cloned
    /// [`EmergencyStop`] still keeps it open.
    pub job_closed: bool,
    /// `true` if at close time the backend held no process handle other
    /// than the run's one, released here.
    pub process_handle_released: bool,
    /// QMP socket file still present after closing (expected `false`).
    pub qmp_socket_left: bool,
}

/// Reconciliation of the run state from process and QMP.
#[derive(Clone, Debug)]
pub struct Reconciliation {
    /// Process state according to the handle (authoritative on liveness).
    pub process: ProcessState,
    /// Exit code read directly from the handle, if exited.
    pub handle_exit_code: Option<u32>,
    /// QMP connection state.
    pub qmp: ConnectionState,
    /// vCPU state according to `query-status`, if QMP responded.
    pub qmp_status: Option<RunState>,
    /// Derived public state.
    pub observed: ObservedState,
    /// Detected anomaly (process alive but QMP lost, guest stopped, ...).
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

/// Started process with QMP connected (internal use: probe and runs).
pub(crate) struct Launched {
    pub(crate) container: Arc<dyn ProcessContainer>,
    pub(crate) child: Arc<ChildProcess>,
    pub(crate) qmp: QmpClient,
    pub(crate) state_tx: Arc<watch::Sender<ProcessState>>,
    pub(crate) flags: Arc<RunFlags>,
}

impl Launched {
    /// Waits for the exit up to `wait`, then terminates the container.
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

/// Removes orphan sockets (QEMU does not delete them after a forced stop or a
/// startup error, F0-02 §4.3).
fn remove_sockets(cmd: &QemuCommand) {
    let _ = std::fs::remove_file(cmd.qmp_socket.as_path());
    if let Some(v) = &cmd.vnc_socket {
        let _ = std::fs::remove_file(v.as_path());
    }
    if let Some(t) = &cmd.tpm {
        let _ = std::fs::remove_file(t.socket.as_path());
    }
}

/// Maximum wait for the swtpm control socket.
const SWTPM_SOCKET_TIMEOUT: Duration = Duration::from_secs(10);

/// Starts swtpm in `container` and waits for its control socket. The socket
/// is only checked for existence, never connected to: with `--terminate`
/// swtpm exits when its first control connection closes, which must be
/// QEMU's. A reaper thread waits for the process so that it never lingers
/// as a zombie counted as active by the container.
async fn start_swtpm(
    container: &Arc<dyn ProcessContainer>,
    run_id: RunId,
    tpm: &TpmCommand,
) -> Result<()> {
    let req = SpawnRequest {
        run_id,
        program: tpm.program.clone(),
        args: tpm.args.clone(),
        env: minimal_environment(),
        stdout: tpm.stdout.clone(),
        stderr: tpm.stderr.clone(),
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
    let deadline = Instant::now() + SWTPM_SOCKET_TIMEOUT;
    loop {
        if let Some(code) = child.try_exit_code()? {
            let mut tail = read_stderr_tail(&tpm.stderr);
            if tail.is_empty() {
                tail = read_stderr_tail(&tpm.log);
            }
            return Err(BackendError::StartupFailed {
                exit_code: Some(code),
                stderr_tail: format!("swtpm exited before QEMU started: {tail}"),
            });
        }
        if tpm.socket.as_path().exists() {
            break;
        }
        if Instant::now() >= deadline {
            let _ = container.kill_all(EMERGENCY_EXIT_CODE);
            return Err(BackendError::StartupFailed {
                exit_code: None,
                stderr_tail: format!(
                    "swtpm control socket not created within {SWTPM_SOCKET_TIMEOUT:?}"
                ),
            });
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    std::thread::Builder::new()
        .name("swtpm-reaper".into())
        .spawn(move || {
            let _ = child.wait(None);
        })
        .map_err(BackendError::Io)?;
    Ok(())
}

fn read_stderr_tail(p: &ManagedPath) -> String {
    let bytes = std::fs::read(p.as_path()).unwrap_or_default();
    let start = bytes.len().saturating_sub(64 * 1024);
    stderr_excerpt(&String::from_utf8_lossy(&bytes[start..]))
}

/// Starts QEMU in the container and connects QMP. The process is terminated if the
/// connection does not succeed within `timeout`.
pub(crate) async fn launch(
    cmd: &QemuCommand,
    qmp_cfg: &QmpConfig,
    timeout: Duration,
) -> Result<Launched> {
    if !cmd.private_dir.as_path().is_dir() {
        return Err(BackendError::invalid(
            "private_dir",
            "the run's private directory does not exist",
        ));
    }
    remove_sockets(cmd);
    let container = new_container(&cmd.limits)?;
    if let Some(tpm) = &cmd.tpm
        && let Err(e) = start_swtpm(&container, cmd.run_id, tpm).await
    {
        remove_sockets(cmd);
        return Err(e);
    }
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
                            "QMP not reachable within {timeout:?} ({e}); {}",
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

/// Thread that waits for the exit on the process handle and publishes the final
/// state with the cause. Before classifying it waits (at most 500 ms) for
/// the QMP reader to deliver the last messages, so the guest's
/// `SHUTDOWN` event is not lost.
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
            // The handle is only needed for the wait: releasing it right away prevents the
            // exited process from staying referenced beyond the close.
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

/// Emergency stop: direct path to the OS primitives, usable
/// from any thread without an async runtime and without going through QMP
/// (ADR-0009).
#[derive(Clone, Debug)]
pub struct EmergencyStop {
    container: Arc<dyn ProcessContainer>,
    child: Arc<ChildProcess>,
    flags: Arc<RunFlags>,
}

impl EmergencyStop {
    /// Requests the container termination and returns immediately.
    pub fn trigger(&self) -> Result<()> {
        self.flags.emergency.store(true, Ordering::SeqCst);
        self.container.kill_all(EMERGENCY_EXIT_CODE)
    }

    /// Terminates and waits for the observed process exit and for the container
    /// to drain. Completion is declared only after both.
    pub fn trigger_and_wait(&self, timeout: Duration) -> Result<(Option<u32>, Duration)> {
        let t0 = Instant::now();
        if self.child.try_exit_code()?.is_none() {
            self.trigger()?;
        }
        let code = self.child.wait(Some(timeout))?;
        if code.is_none() {
            return Err(BackendError::Timeout("QEMU exit after Emergency Stop"));
        }
        while self.container.active_count()? > 0 {
            if t0.elapsed() > timeout {
                return Err(BackendError::Timeout(
                    "container drain after Emergency Stop",
                ));
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        Ok((code, t0.elapsed()))
    }

    /// Identity of the controlled process.
    pub fn identity(&self) -> ProcessIdentity {
        self.child.identity()
    }
}

/// `true` if the exit cause is an orderly shutdown (see
/// [`QemuRun::observation`]): `quit`, Emergency Stop or guest shutdown
/// reported by QMP. Failed startup and unexpected exit are not.
pub fn exit_cause_expected(cause: ExitCause) -> bool {
    match cause {
        ExitCause::EmergencyStop | ExitCause::QmpQuit | ExitCause::GuestShutdown => true,
        ExitCause::StartupFailed | ExitCause::Unexpected => false,
    }
}

/// Check for the absence of NICs on the data of `info network` and `query-pci`
/// (F0-02 §5: `query-rx-filter` is not a reliable indicator).
pub fn check_no_nic(info_network: &str, pci_network: &[PciNetworkDevice]) -> Result<()> {
    let info = info_network.trim();
    if !info.is_empty() || !pci_network.is_empty() {
        return Err(BackendError::UnexpectedNetwork(format!(
            "info network={info:?}, network-class PCI={pci_network:?}"
        )));
    }
    Ok(())
}

/// A VM run.
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
    /// Starts a VM according to `spec`, with paths from `paths`. Returns only with
    /// the process alive, QMP ready, accelerator and absence of NICs verified.
    pub async fn start(
        caps: &HostCapabilities,
        spec: &VmLaunchSpec,
        paths: &dyn ManagedPaths,
        cfg: RunConfig,
    ) -> Result<Self> {
        // ADR-0007: the probe must have verified the machine family and
        // firmware kind of this profile.
        let probed = caps.accelerator_for(spec.profile.probe_target());
        match probed {
            AcceleratorStatus::Available { accelerator, .. }
                if *accelerator == spec.accelerator => {}
            AcceleratorStatus::Available { accelerator, .. } => {
                return Err(BackendError::AcceleratorUnavailable(
                    AcceleratorStatus::Unavailable {
                        accelerator: spec.accelerator,
                        reason: UnavailableReason::ProbeFailed {
                            detail: format!(
                                "the probe verified {} and not {}",
                                accelerator.qemu_name(),
                                spec.accelerator.qemu_name()
                            ),
                        },
                    },
                ));
            }
            AcceleratorStatus::Unavailable { .. } => {
                return Err(BackendError::AcceleratorUnavailable(probed.clone()));
            }
            AcceleratorStatus::NotProbed => {
                let t = spec.profile.probe_target();
                return Err(BackendError::StateConflict(format!(
                    "accelerator not verified with the {} machine and {} firmware: run the \
                     probe first",
                    t.family.label(),
                    t.firmware.label()
                )));
            }
        }
        caps.machine(spec.machine.as_str())?;
        caps.check_cpu(&spec.cpu)?;
        let missing = caps.missing_devices(&spec.profile);
        if !missing.is_empty() {
            return Err(BackendError::Unsupported {
                what: format!("devices {}", missing.join(", ")),
                hint: "the QEMU binary does not include the devices of the hardware profile".into(),
            });
        }
        if spec.profile.firmware() == FirmwareKind::SeaBios {
            // Only the image hashed by the probe, with the verified hash.
            let verified = caps.verified_seabios()?;
            if paths.seabios()?.as_path() != verified.path.as_path() {
                return Err(BackendError::StateConflict(
                    "the run's SeaBIOS image is not the one verified by the probe".into(),
                ));
            }
        }
        if spec.profile.firmware() == FirmwareKind::UefiAarch64 {
            // ADR-0028: only the firmware code hashed by the probe, with an
            // approved hash.
            let verified = caps.verified_aarch64_firmware()?;
            if paths.firmware_code()?.as_path() != verified.code.path.as_path() {
                return Err(BackendError::StateConflict(
                    "the run's AArch64 firmware is not the one verified by the probe".into(),
                ));
            }
        }
        if spec.profile.needs_tpm() {
            let verified = caps.verified_swtpm()?;
            if paths.swtpm()?.as_path() != verified.path.as_path() {
                return Err(BackendError::StateConflict(
                    "the run's swtpm is not the one verified by the probe".into(),
                ));
            }
            let state = paths.tpm_state_dir(&spec.run_id)?.ok_or_else(|| {
                BackendError::invalid("tpm_state", "the TPM profile needs the VM's TPM state")
            })?;
            if !state.as_path().is_dir() {
                return Err(BackendError::invalid(
                    "tpm_state",
                    "the VM's TPM state directory does not exist",
                ));
            }
        }
        let command = spec.build(paths)?;
        if command.program != caps.qemu_system {
            return Err(BackendError::StateConflict(
                "the run's QEMU binary is not the one verified by the probe".into(),
            ));
        }
        let run = &spec.run_id;
        let readonly = paths.protected_readonly(run)?;
        let mut no_delete: Vec<ManagedPath> = paths.firmware_vars(run)?.into_iter().collect();
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

    /// Verifies that the guest has no network interfaces: empty `info network`
    /// and no PCI device of the network class.
    pub async fn verify_no_nic(&self) -> Result<()> {
        let info = self.qmp.network_diagnostics().await?;
        let pci = self.qmp.pci_network_devices().await?;
        check_no_nic(&info, &pci)
    }

    /// Run identifier.
    pub fn run_id(&self) -> RunId {
        self.run_id
    }

    /// Identity of the QEMU process (PID, creation time, `run_id`).
    pub fn identity(&self) -> ProcessIdentity {
        self.identity
    }

    /// Limits applied to the run's Job Object and memory peak.
    pub fn limit_report(&self) -> Result<crate::process::JobLimitReport> {
        self.emergency.container.limit_report()
    }

    /// Accelerator verified at startup.
    pub fn accelerator(&self) -> &AcceleratorsInfo {
        &self.accelerator
    }

    /// Command used (diagnostics).
    pub fn command(&self) -> &QemuCommand {
        &self.command
    }

    /// Files protected by the exclusion during the run.
    pub fn storage_guards(&self) -> &StorageGuards {
        &self.guards
    }

    /// Current process state.
    pub fn state(&self) -> ProcessState {
        self.state_tx.borrow().clone()
    }

    /// Observer of the process state (also detects unexpected exits).
    pub fn subscribe_state(&self) -> watch::Receiver<ProcessState> {
        self.state_tx.subscribe()
    }

    /// QMP events.
    pub fn subscribe_events(&self) -> broadcast::Receiver<QmpEvent> {
        self.qmp.subscribe_events()
    }

    /// Emergency Stop handle, cloneable and independent of the QMP queue.
    pub fn emergency_stop_handle(&self) -> EmergencyStop {
        self.emergency.clone()
    }

    /// Processes active in the run's container.
    pub fn active_processes(&self) -> Result<u32> {
        self.emergency.container.active_count()
    }

    /// Observation for the core (`vm-core`) matching the current state.
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

    /// Expected exit: requested by the backend (quit, Emergency Stop) or
    /// shutdown started by the guest and reported by QMP (`SHUTDOWN` with
    /// `guest: true`), even without a shutdown request from the backend: it is an
    /// orderly shutdown, not a failure (proposal from `vm-core`).
    fn exit_expected(&self, cause: ExitCause) -> bool {
        exit_cause_expected(cause)
    }

    /// Reconciles the state from the process handle and QMP. The handle is
    /// authoritative on process liveness; QMP on the vCPU state.
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
                // Exit already observed on the handle, classification in progress.
                None => (VmState::Stopping, ObservationSource::Process),
            }
        } else {
            match (&process, &qmp, &qmp_status) {
                (ProcessState::Starting, ..) => (VmState::Starting, ObservationSource::Process),
                (ProcessState::Stopping, ..) => (VmState::Stopping, ObservationSource::Process),
                (_, ConnectionState::Closed { reason }, _) => {
                    anomaly = Some(format!("process alive but QMP channel closed: {reason}"));
                    (VmState::Failed, ObservationSource::Qmp)
                }
                (_, _, None) => {
                    anomaly = Some("process alive but QMP not responding".into());
                    (VmState::Failed, ObservationSource::Qmp)
                }
                (_, _, Some(s)) => {
                    match s {
                        RunState::Running | RunState::Paused | RunState::Prelaunch => {}
                        other => anomaly = Some(format!("vCPU state {other:?}")),
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
                "operation not allowed in state {s:?}"
            ))),
        }
    }

    /// Process alive and QMP usable: running, or shutting down after a
    /// graceful stop request (the guest may still show a confirmation that
    /// the user answers from the console).
    fn ensure_alive(&self) -> Result<()> {
        match self.state() {
            ProcessState::Running | ProcessState::Stopping => Ok(()),
            s => Err(BackendError::StateConflict(format!(
                "operation not allowed in state {s:?}"
            ))),
        }
    }

    fn ensure_run(&self, run_id: RunId) -> Result<()> {
        if run_id != self.run_id {
            return Err(BackendError::StateConflict(format!(
                "request refers to run {run_id}, active {}",
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

    /// Current machine.
    pub async fn current_machine(&self) -> Result<CurrentMachine> {
        Ok(self.qmp.query_current_machine().await?)
    }

    /// `query-name`.
    pub async fn guest_name(&self) -> Result<Option<String>> {
        Ok(self.qmp.query_name().await?)
    }

    /// Network diagnostics (`info network`).
    pub async fn network_diagnostics(&self) -> Result<String> {
        Ok(self.qmp.network_diagnostics().await?)
    }

    /// `system_reset`.
    pub async fn reset(&self) -> Result<()> {
        self.ensure_running()?;
        Ok(self.qmp.system_reset().await?)
    }

    /// Screenshot via `screendump` (file chosen by the backend in the private
    /// directory, removed in every case) with the limit from [`RunConfig`].
    pub async fn screenshot(&self) -> Result<CapturedScreenshot> {
        self.screenshot_limited(self.cfg.max_screenshot_bytes).await
    }

    /// Screenshot with an explicit limit on the PNG size.
    pub async fn screenshot_limited(&self, max_png_bytes: u64) -> Result<CapturedScreenshot> {
        Ok(self.screenshot_rgb_limited(max_png_bytes).await?.0)
    }

    /// Screenshot with the decoded RGB image (diagnostics and tests).
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

    /// Geometry of the last screenshot (or of the last
    /// [`Self::refresh_geometry`]), if known.
    pub fn current_geometry(&self) -> Option<DisplayGeometry> {
        *lock(&self.geometry)
    }

    /// Reads the current display geometry with a `screendump` of which only
    /// the PPM header is parsed, and records it as the current geometry
    /// (used to accept input from a console without a prior screenshot
    /// while still rejecting stale coordinates).
    pub async fn refresh_geometry(&self) -> Result<DisplayGeometry> {
        self.ensure_alive()?;
        let g = crate::screenshot::capture_geometry(
            &self.qmp,
            &self.private_dir,
            self.cfg.screendump_timeout,
        )
        .await?;
        *lock(&self.geometry) = Some(g);
        Ok(g)
    }

    /// QEMU display socket of the run (`-vnc unix:`), in the private
    /// directory. Only the supervisor's display proxy connects to it.
    pub fn display_socket(&self) -> Option<&std::path::Path> {
        self.command.vnc_socket.as_ref().map(ManagedPath::as_path)
    }

    /// Forwards input events. The request must refer to this
    /// run and to the geometry of the last screenshot, if known, to
    /// reject stale coordinates (ROADMAP 6.4). No automatic
    /// retry.
    pub async fn send_input(
        &self,
        run_id: RunId,
        geometry: DisplayGeometry,
        events: &[InputEvent],
    ) -> Result<()> {
        self.ensure_run(run_id)?;
        self.ensure_alive()?;
        validate_events(geometry, events)?;
        if let Some(g) = *lock(&self.geometry)
            && g != geometry
        {
            return Err(BackendError::StateConflict(format!(
                "stale geometry {}x{} (current {}x{})",
                geometry.width, geometry.height, g.width, g.height
            )));
        }
        lock(&self.pressed).apply(events);
        Ok(self.qmp.input_send_event(events).await?)
    }

    /// Input batch from `lightvm-proto` (structural validation included).
    pub async fn send_input_batch(&self, batch: &lightvm_proto::InputBatch) -> Result<()> {
        batch.validate()?;
        self.send_input(batch.run_id, batch.geometry, &batch.events)
            .await
    }

    /// `true` if any keys or buttons are pressed.
    pub fn has_pressed_input(&self) -> bool {
        !lock(&self.pressed).is_empty()
    }

    /// Releases all keys and buttons that are pressed (focus loss,
    /// disconnection, change of input owner: ROADMAP 6.4).
    /// Returns the number of release events sent.
    pub async fn release_all_input(&self, run_id: RunId) -> Result<usize> {
        self.ensure_run(run_id)?;
        let events = lock(&self.pressed).take_release_events();
        if events.is_empty() || self.ensure_alive().is_err() {
            return Ok(0);
        }
        self.qmp.input_send_event(&events).await?;
        Ok(events.len())
    }

    /// Waits for the process exit up to `timeout`.
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
            _ => Err(BackendError::Timeout("QEMU exit")),
        }
    }

    /// Graceful shutdown with escalation: `system_powerdown` → wait → `quit` →
    /// wait → container termination (`TerminateJobObject`). Returns
    /// only after the exit observed on the handle.
    pub async fn shutdown(&self, policy: ShutdownPolicy) -> Result<ExitReport> {
        let t0 = Instant::now();
        if matches!(self.state(), ProcessState::Exited { .. }) {
            return self.report(StopPhase::AlreadyExited, t0);
        }
        self.mark_stopping();
        let mut notes = Vec::new();
        // Each phase lasts at most its own timeout, QMP command included:
        // a QEMU that does not respond does not lengthen the escalation.
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
            notes.push("no exit after system_powerdown".into());
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
        notes.push("no exit after quit: terminating the Job Object".into());
        self.emergency.trigger()?;
        if self.wait_exit(policy.kill_timeout).await.is_some() {
            return self.report_with(StopPhase::Kill, t0, notes);
        }
        Err(BackendError::Timeout("QEMU shutdown"))
    }

    /// Emergency Stop: container termination without QMP, completed
    /// only after the observed exit and the container drain.
    pub async fn emergency_stop(&self, timeout: Duration) -> Result<ExitReport> {
        let t0 = Instant::now();
        self.mark_stopping();
        let em = self.emergency.clone();
        tokio::task::spawn_blocking(move || em.trigger_and_wait(timeout))
            .await
            .map_err(|e| BackendError::Io(std::io::Error::other(e.to_string())))??;
        if self.wait_exit(timeout).await.is_none() {
            return Err(BackendError::Timeout("final state after Emergency Stop"));
        }
        self.report(StopPhase::Kill, t0)
    }

    /// Closes the run: QMP client, sockets, handles of the protected files and, if
    /// no cloned [`EmergencyStop`] is still alive, the Job Object. If the
    /// process is still alive the Emergency Stop is run first
    /// (`forced = true` in the report): closing means losing control.
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
        // The only remaining reference to the process handle is the run's one
        // (the waiter thread releases its own before publishing the exit).
        let process_handle_released = Arc::strong_count(&self.emergency.child) == 1;
        let container = self.emergency.container.clone();
        drop(self);
        // Only remaining reference: releasing it closes the Job Object handle.
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
        // Losing the object means losing control of the run:
        // the process is terminated (closing the Job Object with the last
        // reference to the container would do it anyway).
        if !matches!(self.state(), ProcessState::Exited { .. }) {
            let _ = self.emergency.trigger();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nic_check() {
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
    fn expected_exits() {
        assert!(exit_cause_expected(ExitCause::GuestShutdown));
        assert!(exit_cause_expected(ExitCause::QmpQuit));
        assert!(exit_cause_expected(ExitCause::EmergencyStop));
        assert!(!exit_cause_expected(ExitCause::Unexpected));
        assert!(!exit_cause_expected(ExitCause::StartupFailed));
    }
}

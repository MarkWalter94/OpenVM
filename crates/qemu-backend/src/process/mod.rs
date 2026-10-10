//! Containment of the QEMU processes and process identity (ROADMAP 6.1, 7.2
//! point 4, 7.3; ADR-0002, ADR-0009, ADR-0015; F0-06, F0-07).
//!
//! One [`ProcessContainer`] per run:
//!
//! - Windows: anonymous Job Object with `KILL_ON_JOB_CLOSE`, without breakaway;
//!   the process enters it **at creation** (`PROC_THREAD_ATTRIBUTE_JOB_LIST`).
//!   The death of the supervisor closes the job handle and terminates QEMU (F0-06,
//!   scenario a). The Emergency Stop is `TerminateJobObject`.
//! - Linux: signature ready, implementation deferred. F0-07 proposes
//!   `PR_SET_PDEATHSIG(SIGKILL)` set from a dedicated spawn thread plus a
//!   per-VM cgroup v2 with `cgroup.kill` inside a delegated systemd scope;
//!   [`new_container`] currently returns an `unsupported` error.

use std::ffi::OsString;
use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use lightvm_proto::RunId;

use crate::error::Result;
use crate::paths::ManagedPath;

#[cfg(windows)]
mod windows;

/// Effective containment mechanism.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ContainmentMechanism {
    /// Windows Job Object with `KILL_ON_JOB_CLOSE`, process in the job from creation.
    WindowsJobObject,
    /// Linux: delegated cgroup v2 with `cgroup.kill` plus `PR_SET_PDEATHSIG` (deferred).
    LinuxCgroupPdeathsig,
}

/// Serializable process identity: PID plus creation time. The PID alone
/// can be reassigned (F0-06, section 3); the primary identity remains
/// the handle owned by [`ChildProcess`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ProcessIdentity {
    /// Run the process belongs to.
    pub run_id: RunId,
    /// PID (informational).
    pub pid: u32,
    /// Creation time (Windows: FILETIME in 100 ns units).
    pub creation_time: u64,
}

/// State of a process looked up by identity (PID + creation time).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IdentityStatus {
    /// No process with that PID.
    NotFound,
    /// The PID has been reassigned to another process.
    Reused,
    /// Same process, exited but still referenced by a handle.
    Exited,
    /// Same process, running.
    Alive,
}

impl IdentityStatus {
    /// `true` if the process with this identity is still running.
    pub fn is_alive(self) -> bool {
        matches!(self, Self::Alive)
    }
}

impl ProcessIdentity {
    /// Looks up the process with this identity without touching it (check for
    /// absence of orphans, reconciliation after a supervisor restart).
    pub fn status(&self) -> Result<IdentityStatus> {
        #[cfg(windows)]
        {
            use crate::sys::windows::{PidStatus, pid_status};
            Ok(match pid_status(self.pid, self.creation_time)? {
                PidStatus::NotFound => IdentityStatus::NotFound,
                PidStatus::Reused => IdentityStatus::Reused,
                PidStatus::Exited => IdentityStatus::Exited,
                PidStatus::Alive => IdentityStatus::Alive,
            })
        }
        #[cfg(not(windows))]
        {
            Err(crate::error::BackendError::Unsupported {
                what: "process identity verification on this system".into(),
                hint: "Linux implementation (pidfd) not yet available".into(),
            })
        }
    }
}

/// Exit code used by the Emergency Stop with `TerminateJobObject`.
pub const EMERGENCY_EXIT_CODE: u32 = 0xE5E5;

/// Class of the QEMU exit code on Windows (F0-06 §2b, F0-08).
///
/// The code alone does not always distinguish the cause (`taskkill /F` yields 1,
/// like a QEMU error): the definitive cause combines code and backend
/// actions (see [`crate::run::ExitCause`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExitCodeClass {
    /// 0: orderly exit (`quit`, guest shutdown, job closure).
    Clean,
    /// 1: QEMU error (command line, accelerator) or `taskkill /F`.
    QemuErrorOrForcedKill,
    /// [`EMERGENCY_EXIT_CODE`]: job termination requested by the backend.
    EmergencyStop,
    /// `0xFFFFFFFF`: external `TerminateProcess(-1)` (for example `Stop-Process`).
    ExternalTermination,
    /// Error NTSTATUS (`0xC0000000` and above): exception or crash, including
    /// exceeding the job memory limit (F0-08).
    Exception(u32),
    /// Other code.
    Other(u32),
    /// Code not available.
    Unknown,
}

/// Classifies an exit code.
pub fn classify_exit_code(code: Option<u32>) -> ExitCodeClass {
    match code {
        None => ExitCodeClass::Unknown,
        Some(0) => ExitCodeClass::Clean,
        Some(1) => ExitCodeClass::QemuErrorOrForcedKill,
        Some(EMERGENCY_EXIT_CODE) => ExitCodeClass::EmergencyStop,
        Some(0xFFFF_FFFF) => ExitCodeClass::ExternalTermination,
        Some(c) if c >= 0xC000_0000 => ExitCodeClass::Exception(c),
        Some(c) => ExitCodeClass::Other(c),
    }
}

/// Request to start a process in the container.
#[derive(Clone, Debug)]
pub struct SpawnRequest {
    /// Run the process will belong to.
    pub run_id: RunId,
    /// Approved executable.
    pub program: ManagedPath,
    /// Structured arguments.
    pub args: Vec<OsString>,
    /// Complete environment of the child (nothing inherited from the supervisor).
    pub env: Vec<(OsString, OsString)>,
    /// File receiving stdout.
    pub stdout: ManagedPath,
    /// File receiving stderr.
    pub stderr: ManagedPath,
}

/// Minimal environment for QEMU on this host.
///
/// F0-06 (1b): with an empty environment Winsock does not initialise and QEMU does not create the
/// QMP socket; with only `SystemRoot` QMP and WHPX work. `PATH`, `TEMP`,
/// `USERPROFILE`, `APPDATA` are not needed with firmware at absolute paths and
/// `-no-user-config`. No credential (provider, MCP client) is passed on.
pub fn minimal_environment() -> Vec<(OsString, OsString)> {
    let mut env = Vec::new();
    if cfg!(windows) {
        let root = std::env::var_os("SystemRoot").unwrap_or_else(|| OsString::from(r"C:\Windows"));
        env.push((OsString::from("SystemRoot"), root));
    }
    env
}

/// Container of the processes of a run.
pub trait ProcessContainer: Send + Sync + fmt::Debug {
    /// Starts the process already inside the container (no time window
    /// in which the process is outside the container).
    fn spawn(&self, req: &SpawnRequest) -> Result<ChildProcess>;

    /// Terminates all processes of the container with OS primitives, without QMP
    /// (Emergency Stop, ADR-0009). Does not wait: see [`Self::active_count`].
    fn kill_all(&self, exit_code: u32) -> Result<()>;

    /// Processes still active in the container.
    fn active_count(&self) -> Result<u32>;

    /// Effective mechanism.
    fn mechanism(&self) -> ContainmentMechanism;

    /// Verifies that the process belongs to the container and that the
    /// container terminates the processes when it is closed.
    fn verify_membership(&self, child: &ChildProcess) -> Result<()>;

    /// Applied limits and memory peak.
    fn limit_report(&self) -> Result<JobLimitReport>;
}

/// Container limits, applied before the spawn (ADR-0017).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct JobLimits {
    /// Hard CPU cap in hundredths of a percentage point of all logical
    /// processors (1..=10000; 2500 = 25 % of the machine).
    pub cpu_rate: Option<u32>,
    /// Maximum memory of the whole container in bytes (QEMU included).
    pub job_memory_bytes: Option<u64>,
}

impl JobLimits {
    /// Validation of the values.
    pub fn validate(&self) -> Result<()> {
        if let Some(r) = self.cpu_rate
            && !(1..=10_000).contains(&r)
        {
            return Err(crate::error::BackendError::invalid(
                "cpu_rate",
                "CPU cap outside the range 1..=10000",
            ));
        }
        if self.job_memory_bytes == Some(0) {
            return Err(crate::error::BackendError::invalid(
                "job_memory_bytes",
                "zero memory limit",
            ));
        }
        Ok(())
    }
}

/// Limits read from the container and observed memory peak.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct JobLimitReport {
    /// Active CPU cap.
    pub cpu_rate: Option<u32>,
    /// Active maximum memory in bytes.
    pub job_memory_bytes: Option<u64>,
    /// Container memory peak in bytes.
    pub peak_job_memory_bytes: u64,
}

impl JobLimitReport {
    /// `true` if the peak reached the memory limit (likely exit
    /// due to exceeding the limit: the job refuses allocations beyond it).
    pub fn memory_limit_reached(&self) -> bool {
        self.job_memory_bytes
            .is_some_and(|l| self.peak_job_memory_bytes.saturating_add(16 << 20) >= l)
    }
}

/// Creates the container for a new run with the given limits.
pub fn new_container(limits: &JobLimits) -> Result<Arc<dyn ProcessContainer>> {
    limits.validate()?;
    #[cfg(windows)]
    {
        Ok(Arc::new(windows::JobContainer::new(limits)?))
    }
    #[cfg(not(windows))]
    {
        let _ = limits;
        // TODO(F1/R2a): delegated cgroup v2 + `cgroup.kill` and
        // `PR_SET_PDEATHSIG(SIGKILL)` from a dedicated spawn thread (F0-07).
        Err(crate::error::BackendError::Unsupported {
            what: "containment of QEMU processes on this operating system".into(),
            hint: "Linux implementation (cgroup v2 + PR_SET_PDEATHSIG, F0-07) not yet available"
                .into(),
        })
    }
}

/// Owned child process: the handle is the primary identity.
#[derive(Debug)]
pub struct ChildProcess {
    identity: ProcessIdentity,
    #[cfg(windows)]
    handle: std::os::windows::io::OwnedHandle,
}

impl ChildProcess {
    /// Serializable identity.
    pub fn identity(&self) -> ProcessIdentity {
        self.identity
    }

    /// Waits for exit up to `timeout` (`None` = no limit). Returns
    /// the exit code if the process has exited.
    pub fn wait(&self, timeout: Option<Duration>) -> Result<Option<u32>> {
        #[cfg(windows)]
        {
            let ms = timeout.map(|t| u32::try_from(t.as_millis()).unwrap_or(u32::MAX - 1));
            Ok(crate::sys::windows::wait_process(&self.handle, ms)?)
        }
        #[cfg(not(windows))]
        {
            let _ = timeout;
            Err(crate::error::BackendError::StateConflict(
                "process not managed on this system".into(),
            ))
        }
    }

    /// Exit code if the process has already exited.
    pub fn try_exit_code(&self) -> Result<Option<u32>> {
        self.wait(Some(Duration::ZERO))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exit_code_classes() {
        assert_eq!(classify_exit_code(None), ExitCodeClass::Unknown);
        assert_eq!(classify_exit_code(Some(0)), ExitCodeClass::Clean);
        assert_eq!(
            classify_exit_code(Some(1)),
            ExitCodeClass::QemuErrorOrForcedKill
        );
        assert_eq!(
            classify_exit_code(Some(0xE5E5)),
            ExitCodeClass::EmergencyStop
        );
        assert_eq!(
            classify_exit_code(Some(0xFFFF_FFFF)),
            ExitCodeClass::ExternalTermination
        );
        assert_eq!(
            classify_exit_code(Some(0xC000_0409)),
            ExitCodeClass::Exception(0xC000_0409)
        );
        assert_eq!(classify_exit_code(Some(3)), ExitCodeClass::Other(3));
    }
}

//! Unix container (macOS, Linux without delegated cgroups; ADR-0028):
//! process group plus a lifeline watchdog.
//!
//! The container starts a watchdog (`/bin/sh`) as the leader of a new
//! process group, reading a pipe whose write end only the supervisor holds
//! (close-on-exec, never inherited by a child). The processes of the run
//! are spawned into that group. When the supervisor dies, for any cause
//! `SIGKILL` included, the kernel closes the write end: the watchdog reads
//! end-of-file and sends `SIGKILL` to the whole group, itself included.
//! The Emergency Stop sends `SIGKILL` to the group directly.
//!
//! Limits: no per-tree CPU or memory limits (no cgroups); requested limits
//! are not applied and the report says so (agent mode stays disabled on
//! these hosts, ADR-0028). A descendant that leaves the group, or a
//! `SIGKILL` sent to the watchdog alone, is not contained.

use std::io::PipeWriter;
use std::os::unix::process::CommandExt as _;
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use rustix::process::{Pid, Signal};

use super::{
    ChildProcess, ContainmentMechanism, JobLimitReport, JobLimits, ProcessContainer,
    ProcessIdentity, SpawnRequest,
};
use crate::error::{BackendError, Result};

/// Watchdog: waits for end-of-file on stdin (the lifeline), then kills its
/// own process group. Terminal signals are ignored: only the lifeline or a
/// direct `SIGKILL` ends it.
const WATCHDOG_SCRIPT: &str =
    "trap '' HUP INT QUIT TERM; while read -r _; do :; done; kill -KILL 0";

#[derive(Debug)]
pub(super) struct GroupContainer {
    /// Process group of the run (the watchdog's PID).
    pgid: Pid,
    watchdog: Mutex<Child>,
    /// Write end of the lifeline: dropping it (container dropped or
    /// supervisor dead) ends the group.
    lifeline: Mutex<Option<PipeWriter>>,
    /// PIDs spawned into the group, for the active count.
    pids: Mutex<Vec<u32>>,
    requested: JobLimits,
}

impl GroupContainer {
    pub(super) fn new(limits: &JobLimits) -> Result<Self> {
        let (reader, writer) = std::io::pipe()?;
        let watchdog = Command::new("/bin/sh")
            .args(["-c", WATCHDOG_SCRIPT])
            .env_clear()
            .stdin(Stdio::from(reader))
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(0)
            .spawn()
            .map_err(BackendError::Spawn)?;
        let pgid = Pid::from_raw(i32::try_from(watchdog.id()).unwrap_or(0))
            .ok_or_else(|| BackendError::StateConflict("watchdog without a PID".into()))?;
        Ok(Self {
            pgid,
            watchdog: Mutex::new(watchdog),
            lifeline: Mutex::new(Some(writer)),
            pids: Mutex::new(Vec::new()),
            requested: *limits,
        })
    }

    fn alive(pid: u32) -> bool {
        i32::try_from(pid)
            .ok()
            .and_then(Pid::from_raw)
            .is_some_and(|p| rustix::process::test_kill_process(p).is_ok())
    }
}

impl Drop for GroupContainer {
    fn drop(&mut self) {
        // Closing the lifeline ends the group; reap the watchdog.
        if let Ok(mut l) = self.lifeline.lock() {
            l.take();
        }
        if let Ok(mut w) = self.watchdog.lock() {
            let _ = w.wait();
        }
    }
}

impl ProcessContainer for GroupContainer {
    fn spawn(&self, req: &SpawnRequest) -> Result<ChildProcess> {
        if self
            .lifeline
            .lock()
            .map_err(|_| BackendError::StateConflict("container lock poisoned".into()))?
            .is_none()
        {
            return Err(BackendError::StateConflict("container closed".into()));
        }
        let stdout = std::fs::File::create(req.stdout.as_path())?;
        let stderr = std::fs::File::create(req.stderr.as_path())?;
        let mut cmd = Command::new(req.program.as_path());
        cmd.args(&req.args)
            .env_clear()
            .envs(req.env.iter().map(|(k, v)| (k, v)))
            .stdin(Stdio::null())
            .stdout(stdout)
            .stderr(stderr)
            .process_group(self.pgid.as_raw_nonzero().get());
        let child = cmd.spawn().map_err(BackendError::Spawn)?;
        let pid = child.id();
        self.pids
            .lock()
            .map_err(|_| BackendError::StateConflict("container lock poisoned".into()))?
            .push(pid);
        Ok(ChildProcess {
            identity: ProcessIdentity {
                run_id: req.run_id,
                pid,
                creation_time: 0,
            },
            child: Mutex::new(child),
        })
    }

    fn kill_all(&self, _exit_code: u32) -> Result<()> {
        match rustix::process::kill_process_group(self.pgid, Signal::KILL) {
            Ok(()) | Err(rustix::io::Errno::SRCH) => Ok(()),
            // macOS: EPERM while only zombies are left in the group.
            Err(rustix::io::Errno::PERM) => Ok(()),
            Err(e) => Err(BackendError::Io(e.into())),
        }
    }

    fn active_count(&self) -> Result<u32> {
        let pids = self
            .pids
            .lock()
            .map_err(|_| BackendError::StateConflict("container lock poisoned".into()))?;
        Ok(u32::try_from(pids.iter().filter(|&&p| Self::alive(p)).count()).unwrap_or(u32::MAX))
    }

    fn mechanism(&self) -> ContainmentMechanism {
        ContainmentMechanism::UnixProcessGroupWatchdog
    }

    fn verify_membership(&self, child: &ChildProcess) -> Result<()> {
        let pid = i32::try_from(child.identity.pid)
            .ok()
            .and_then(Pid::from_raw)
            .ok_or_else(|| BackendError::StateConflict("invalid PID".into()))?;
        let pgid = rustix::process::getpgid(Some(pid)).map_err(|e| BackendError::Io(e.into()))?;
        if pgid != self.pgid {
            return Err(BackendError::StateConflict(format!(
                "process {} outside the container group",
                child.identity.pid
            )));
        }
        let mut w = self
            .watchdog
            .lock()
            .map_err(|_| BackendError::StateConflict("container lock poisoned".into()))?;
        if w.try_wait()?.is_some() {
            return Err(BackendError::StateConflict(
                "the container watchdog is not running".into(),
            ));
        }
        Ok(())
    }

    fn limit_report(&self) -> Result<JobLimitReport> {
        // No limit is applied on this mechanism.
        let _ = self.requested;
        Ok(JobLimitReport::default())
    }
}

/// Waits for `child` up to `timeout` (`None` = no limit). Exit code: the
/// process's own code, or 128 + signal number when killed by a signal.
pub(super) fn wait_child(child: &Mutex<Child>, timeout: Option<Duration>) -> Result<Option<u32>> {
    use std::os::unix::process::ExitStatusExt as _;
    let deadline = timeout.map(|t| Instant::now() + t);
    loop {
        let status = child
            .lock()
            .map_err(|_| BackendError::StateConflict("child lock poisoned".into()))?
            .try_wait()?;
        if let Some(s) = status {
            let code = s
                .code()
                .map(|c| c as u32)
                .or_else(|| s.signal().map(|n| 128 + n as u32));
            return Ok(code);
        }
        if deadline.is_some_and(|d| Instant::now() >= d) {
            return Ok(None);
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Status of a PID without a handle: no creation time on this mechanism,
/// so a reused PID is reported as alive (conservative).
pub(super) fn pid_alive(pid: u32) -> bool {
    GroupContainer::alive(pid)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paths::ManagedPath;
    use lightvm_proto::RunId;

    fn sleeper(c: &GroupContainer, dir: &std::path::Path) -> ChildProcess {
        let req = SpawnRequest {
            run_id: RunId::new_v7(),
            program: ManagedPath::verify("/bin/sleep").unwrap(),
            args: vec!["30".into()],
            env: Vec::new(),
            stdout: ManagedPath::verify(dir.join("out.log")).unwrap(),
            stderr: ManagedPath::verify(dir.join("err.log")).unwrap(),
        };
        c.spawn(&req).unwrap()
    }

    fn temp() -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!(
            "lvqb-proc-{}-{}",
            std::process::id(),
            RunId::new_v7()
        ));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn emergency_stop_kills_the_group() {
        let d = temp();
        let c = GroupContainer::new(&JobLimits::default()).unwrap();
        let a = sleeper(&c, &d);
        let b = sleeper(&c, &d);
        c.verify_membership(&a).unwrap();
        assert_eq!(c.active_count().unwrap(), 2);
        c.kill_all(0).unwrap();
        assert_eq!(a.wait(Some(Duration::from_secs(5))).unwrap(), Some(137));
        assert_eq!(b.wait(Some(Duration::from_secs(5))).unwrap(), Some(137));
        assert_eq!(c.active_count().unwrap(), 0);
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn closing_the_lifeline_kills_the_group() {
        let d = temp();
        let c = GroupContainer::new(&JobLimits::default()).unwrap();
        let a = sleeper(&c, &d);
        let t0 = Instant::now();
        // Same effect as the supervisor dying: the write end is closed.
        c.lifeline.lock().unwrap().take();
        assert_eq!(a.wait(Some(Duration::from_secs(5))).unwrap(), Some(137));
        assert!(t0.elapsed() < Duration::from_secs(2), "{:?}", t0.elapsed());
        assert!(
            c.spawn(&SpawnRequest {
                run_id: RunId::new_v7(),
                program: ManagedPath::verify("/bin/sleep").unwrap(),
                args: vec!["1".into()],
                env: Vec::new(),
                stdout: ManagedPath::verify(d.join("o2.log")).unwrap(),
                stderr: ManagedPath::verify(d.join("e2.log")).unwrap(),
            })
            .is_err()
        );
        let _ = std::fs::remove_dir_all(d);
    }
}

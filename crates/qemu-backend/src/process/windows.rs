//! Contenitore Windows: Job Object per esecuzione (F0-06).

use crate::error::{BackendError, Result};
use crate::sys::windows::{self as sys, ConsoleMode, Job};

use super::{ChildProcess, ContainmentMechanism, ProcessContainer, ProcessIdentity, SpawnRequest};

/// Job Object anonimo con `KILL_ON_JOB_CLOSE`; QEMU senza console
/// (`DETACHED_PROCESS`, nessun `conhost.exe`: decisione proposta in F0-06).
#[derive(Debug)]
pub(crate) struct JobContainer {
    job: Job,
    console: ConsoleMode,
}

impl JobContainer {
    pub(crate) fn new() -> Result<Self> {
        Ok(Self {
            job: Job::create().map_err(BackendError::Spawn)?,
            console: ConsoleMode::Detached,
        })
    }
}

impl ProcessContainer for JobContainer {
    fn spawn(&self, req: &SpawnRequest) -> Result<ChildProcess> {
        let exe = req.program.as_path();
        let spawned = sys::spawn(&sys::SpawnSpec {
            exe,
            args: &req.args,
            env: &req.env,
            stdout: req.stdout.as_path(),
            stderr: req.stderr.as_path(),
            job: &self.job,
            console: self.console,
            cwd: exe.parent(),
        })
        .map_err(BackendError::Spawn)?;
        let creation_time = match sys::creation_time(&spawned.process) {
            Ok(t) => t,
            Err(e) => {
                // Senza identità il processo non è gestibile: lo si termina.
                let _ = self.job.terminate(1);
                return Err(BackendError::Spawn(e));
            }
        };
        Ok(ChildProcess {
            identity: ProcessIdentity {
                pid: spawned.pid,
                creation_time,
            },
            handle: spawned.process,
        })
    }

    fn kill_all(&self, exit_code: u32) -> Result<()> {
        self.job.terminate(exit_code).map_err(BackendError::Io)
    }

    fn active_count(&self) -> Result<u32> {
        Ok(self.job.accounting()?.active_processes)
    }

    fn mechanism(&self) -> ContainmentMechanism {
        ContainmentMechanism::WindowsJobObject
    }

    fn verify_membership(&self, child: &ChildProcess) -> Result<()> {
        if !self.job.contains(&child.handle)? {
            return Err(BackendError::StateConflict(
                "il processo QEMU non appartiene al Job Object".into(),
            ));
        }
        let flags = self.job.limit_flags()?;
        if flags & sys::KILL_ON_JOB_CLOSE == 0 {
            return Err(BackendError::StateConflict(
                "Job Object senza KILL_ON_JOB_CLOSE".into(),
            ));
        }
        let acc = self.job.accounting()?;
        if acc.total_processes == 0 {
            return Err(BackendError::StateConflict(
                "Job Object senza processi".into(),
            ));
        }
        Ok(())
    }
}

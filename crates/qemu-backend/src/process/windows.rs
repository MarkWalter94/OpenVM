//! Windows container: one Job Object per run (F0-06).

use crate::error::{BackendError, Result};
use crate::sys::windows::{self as sys, ConsoleMode, Job};

use super::{
    ChildProcess, ContainmentMechanism, JobLimitReport, JobLimits, ProcessContainer,
    ProcessIdentity, SpawnRequest,
};

/// Anonymous Job Object with `KILL_ON_JOB_CLOSE`; QEMU without a console
/// (`DETACHED_PROCESS`, no `conhost.exe`: decision proposed in F0-06).
#[derive(Debug)]
pub(crate) struct JobContainer {
    job: Job,
    console: ConsoleMode,
}

impl JobContainer {
    pub(crate) fn new(limits: &JobLimits) -> Result<Self> {
        Ok(Self {
            job: Job::create(limits.cpu_rate, limits.job_memory_bytes)
                .map_err(BackendError::Spawn)?,
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
            // Keep library caches in the run's output directory, never in
            // the hash-approved runtime bundle.
            cwd: req.stdout.as_path().parent(),
        })
        .map_err(BackendError::Spawn)?;
        let creation_time = match sys::creation_time(&spawned.process) {
            Ok(t) => t,
            Err(e) => {
                // Without an identity the process cannot be managed: terminate it.
                let _ = self.job.terminate(1);
                return Err(BackendError::Spawn(e));
            }
        };
        Ok(ChildProcess {
            identity: ProcessIdentity {
                run_id: req.run_id,
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
                "the QEMU process does not belong to the Job Object".into(),
            ));
        }
        let flags = self.job.limit_flags()?;
        if flags & sys::KILL_ON_JOB_CLOSE == 0 {
            return Err(BackendError::StateConflict(
                "Job Object without KILL_ON_JOB_CLOSE".into(),
            ));
        }
        let acc = self.job.accounting()?;
        if acc.total_processes == 0 {
            return Err(BackendError::StateConflict(
                "Job Object without processes".into(),
            ));
        }
        Ok(())
    }

    fn limit_report(&self) -> Result<JobLimitReport> {
        let i = self.job.limit_info()?;
        Ok(JobLimitReport {
            cpu_rate: i.cpu_rate,
            job_memory_bytes: i.job_memory,
            peak_job_memory_bytes: i.peak_job_memory,
        })
    }
}

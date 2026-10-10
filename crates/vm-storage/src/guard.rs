//! Disk protection during a run (ADR-0025, backlog F1-09).
//!
//! QEMU's image locking does not protect on Windows (`F0-02` §8.3,
//! `F0-05` §7.1). The catalog therefore keeps guard handles, independent of
//! QEMU, open for the whole duration of the run:
//!
//! - **baseline** (base disk and baseline vars): read with `FILE_SHARE_READ`
//!   only. While the handle is open no other process can write, rename or
//!   delete them; QEMU reads them. If opening fails with a sharing violation,
//!   someone already holds them open for writing (**writer detector**): error
//!   [`StorageError::VmBusy`], the start must be refused. After opening, the
//!   SHA-256 is recomputed **by reading through the same handle** and compared
//!   with the catalog: the content cannot change between verification and
//!   QEMU start. A missing read-only attribute is restored and reported.
//! - **run overlay and vars**: read with
//!   `FILE_SHARE_READ | FILE_SHARE_WRITE`, without `FILE_SHARE_DELETE`: prevents
//!   rename and deletion while in use, lets QEMU write.
//!
//! **Declared limit (open decision, ADR-0025 item 4):** the share mode is a
//! property of the open, not of the process; QEMU opens the overlay with
//! `FILE_SHARE_WRITE`, so another process of the same user can write the
//! overlay in use. The additional protection (per-run directory with an
//! explicit ACL, a distinct process identity for QEMU, or a permanently
//! declared limit) is a user decision not yet taken; this module does not
//! implement it.
//!
//! Other limits: protection ends when the handles are closed (end of the run
//! or death of the supervisor); it does not protect against an administrator,
//! volume-level access or memory mappings; on non-Windows systems the files
//! are opened without exclusion.

use std::fs::File;

use crate::catalog::Catalog;
use crate::error::{Result, StorageError};
use crate::fsx::{self, Share};
use crate::ids::{BaselineId, RunId};
use crate::paths::ManagedPath;

/// File held open by a guard handle.
#[derive(Debug)]
struct GuardedFile {
    path: ManagedPath,
    _file: File,
}

/// Result of the checks performed when a guard is acquired.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GuardReport {
    /// Files whose SHA-256 was recomputed through the guard handle and found
    /// equal to the catalog.
    pub hash_verified: Vec<ManagedPath>,
    /// Baseline files without a recorded hash (verification not possible).
    pub hash_not_recorded: Vec<ManagedPath>,
    /// Baseline files found without the read-only attribute and fixed.
    pub readonly_restored: Vec<ManagedPath>,
}

/// Guard on the baseline: base disk and vars opened with `FILE_SHARE_READ`
/// only. The protection lasts as long as the value is alive.
#[derive(Debug)]
pub struct BaselineGuard {
    baseline_id: BaselineId,
    files: Vec<GuardedFile>,
    report: GuardReport,
}

impl BaselineGuard {
    /// Protected baseline.
    pub fn baseline_id(&self) -> BaselineId {
        self.baseline_id
    }

    /// Files protected as read-only.
    pub fn paths(&self) -> impl Iterator<Item = &ManagedPath> {
        self.files.iter().map(|f| &f.path)
    }

    /// Checks performed at acquisition.
    pub fn report(&self) -> &GuardReport {
        &self.report
    }
}

/// Guard of a run: read-only baseline plus the run's overlay and vars
/// protected against rename and deletion.
#[derive(Debug)]
pub struct RunGuard {
    run_id: RunId,
    baseline: Option<BaselineGuard>,
    no_delete: Vec<GuardedFile>,
}

impl RunGuard {
    /// Protected run.
    pub fn run_id(&self) -> RunId {
        self.run_id
    }

    /// Guard of the starting baseline, if the run has one.
    pub fn baseline(&self) -> Option<&BaselineGuard> {
        self.baseline.as_ref()
    }

    /// Files protected only against rename and deletion (run overlay and
    /// vars). They remain writable by other processes of the same user
    /// (declared limit).
    pub fn no_delete_paths(&self) -> impl Iterator<Item = &ManagedPath> {
        self.no_delete.iter().map(|f| &f.path)
    }
}

/// Result of the writer detector on a baseline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WriterProbe {
    /// No incompatible handle: the exclusive open succeeded.
    Clear,
    /// At least one baseline file is open by another process in a mode
    /// incompatible with shared read-only access (typically for writing).
    Busy(Vec<ManagedPath>),
}

impl Catalog {
    /// Opens the baseline's base disk and vars with `FILE_SHARE_READ` only,
    /// recomputes the SHA-256 through the handles and compares it with the
    /// catalog, restores the read-only attribute if missing.
    ///
    /// Errors: [`StorageError::VmBusy`] if another process holds a file open
    /// in an incompatible mode (writer detector);
    /// [`StorageError::IntegrityMismatch`] if the content does not match;
    /// [`StorageError::InvalidPath`] if a file has been replaced by a link.
    pub fn guard_baseline(&mut self, id: &BaselineId) -> Result<BaselineGuard> {
        self.baseline_guard(id, true)
    }

    /// Writer detector: attempts the exclusive open of the baseline and
    /// releases it immediately. Does not verify hashes.
    pub fn probe_baseline_writers(&mut self, id: &BaselineId) -> Result<WriterProbe> {
        let files = self.baseline_files(id)?;
        let mut busy = Vec::new();
        for (rel, _) in files {
            let abs = self.layout.resolve(&rel)?;
            match fsx::open_regular(&abs, Share::ReadOnly, rel.as_str()) {
                Ok(_) => {}
                Err(StorageError::VmBusy(_)) => busy.push(rel),
                Err(e) => return Err(e),
            }
        }
        Ok(if busy.is_empty() {
            WriterProbe::Clear
        } else {
            WriterProbe::Busy(busy)
        })
    }

    /// Guard for starting an already prepared run: call it before starting
    /// QEMU and keep it alive until the process ends. Verifies the baseline
    /// hash through the handles (on every start) and opens the run's overlay
    /// and vars without `FILE_SHARE_DELETE`.
    pub fn guard_run(&mut self, run_id: &RunId) -> Result<RunGuard> {
        let (run, overlay, vars) = self.read(|tx| {
            let run = tx.run(run_id)?;
            let overlay = run
                .overlay_disk_id
                .as_ref()
                .map(|d| tx.disk(d))
                .transpose()?;
            let vars = run
                .vars_fw_id
                .as_ref()
                .map(|f| tx.firmware(f))
                .transpose()?;
            Ok((run, overlay, vars))
        })?;
        if run.ended_at.is_some() {
            return Err(StorageError::StateConflict(format!(
                "run {run_id} already ended"
            )));
        }
        let baseline = match &run.baseline_id {
            Some(b) => Some(self.baseline_guard(b, true)?),
            None => None,
        };
        let mut no_delete = Vec::new();
        let paths = overlay
            .map(|d| d.managed_path)
            .into_iter()
            .chain(vars.map(|f| f.managed_path));
        for rel in paths {
            let abs = self.layout.resolve(&rel)?;
            let file = fsx::open_regular(&abs, Share::NoDelete, rel.as_str())?;
            no_delete.push(GuardedFile {
                path: rel,
                _file: file,
            });
        }
        Ok(RunGuard {
            run_id: *run_id,
            baseline,
            no_delete,
        })
    }

    /// Baseline guard with optional hash verification (preparing a run uses
    /// only the writer detector; full verification happens in
    /// [`Self::guard_run`], before start).
    pub(crate) fn baseline_guard(
        &mut self,
        id: &BaselineId,
        verify: bool,
    ) -> Result<BaselineGuard> {
        let files = self.baseline_files(id)?;
        let mut guard = BaselineGuard {
            baseline_id: *id,
            files: Vec::new(),
            report: GuardReport::default(),
        };
        for (rel, sha) in files {
            let abs = self.layout.resolve(&rel)?;
            let file = fsx::open_regular(&abs, Share::ReadOnly, rel.as_str())?;
            if verify {
                match sha {
                    Some(expected) => {
                        let (actual, _) = fsx::sha256_handle(&file, &abs)?;
                        if actual != expected {
                            return Err(StorageError::IntegrityMismatch {
                                path: rel.to_string(),
                                detail: format!("expected SHA-256 {expected}, found {actual}"),
                            });
                        }
                        guard.report.hash_verified.push(rel.clone());
                    }
                    None => guard.report.hash_not_recorded.push(rel.clone()),
                }
            }
            if fsx::ensure_readonly(&abs)? {
                guard.report.readonly_restored.push(rel.clone());
            }
            guard.files.push(GuardedFile {
                path: rel,
                _file: file,
            });
        }
        Ok(guard)
    }

    /// Base disk and vars of a baseline with the recorded hash.
    fn baseline_files(&mut self, id: &BaselineId) -> Result<Vec<(ManagedPath, Option<String>)>> {
        self.read(|tx| {
            let b = tx.baseline(id)?;
            let disk = tx.disk(&b.base_disk_id)?;
            let mut out = vec![(disk.managed_path, disk.sha256)];
            if let Some(f) = &b.vars_fw_id {
                let fw = tx.firmware(f)?;
                out.push((fw.managed_path, fw.sha256));
            }
            Ok(out)
        })
    }
}

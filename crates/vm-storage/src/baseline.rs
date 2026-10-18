//! Baselines with the VM off, current state and restore (F3; ROADMAP 8,
//! ADR-0005, ADR-0025).
//!
//! Model:
//! - the **current state** of a VM is either its standalone working disk
//!   and UEFI variables (F2, files produced by the backend and not
//!   registered in the catalog) or, after a restore, an **overlay** of a
//!   baseline plus a writable copy of the baseline's UEFI variables
//!   (`vm_current_state`, schema v6);
//! - a **baseline** is a standalone, read-only image of the current state
//!   (the caller flattens it with `qemu-img convert`: no backing chain) plus
//!   a read-only copy of the UEFI variables, with a versioned manifest and
//!   SHA-256 hashes;
//! - a **restore** replaces the current state with a new overlay backed by
//!   the baseline (fixed relative reference, one level:
//!   [`CatalogLayout::overlay_backing_ref`]) and a fresh copy of the
//!   baseline's variables; the previous state is discarded.
//!
//! Long operations run in three steps so that the file work happens outside
//! the thread that owns the catalog: `begin_*` (checks, guards, journal
//! intents, on the catalog), `*Job::run` (file work, no catalog: another
//! thread), `finish_*` (publication and one transaction, on the catalog), or
//! [`Catalog::abort_baseline_job`] / [`Catalog::abort_restore_job`].
//!
//! Crash safety: every file goes through the intent journal of the
//! operation. A crash at any point is reconciled at the next open
//! ([`Catalog::recover`]): an uncommitted creation is removed, an uncommitted
//! restore puts the discarded files back (`discard_file` intents), a baseline
//! deletion rolls forward once a file was moved. Fault injection
//! ([`FaultPoint`]) reproduces each critical point in tests.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use crate::catalog::{Catalog, Intent, IntentSpec, exists_no_follow};
use crate::enums::{DiskRole, DiskStatus, FirmwareScope, IntentKind, Lifecycle, TargetKind};
use crate::error::{PathRejection, Result, StorageError};
use crate::fsx::{self, Share};
use crate::guard::{BaselineGuard, RunGuard};
use crate::ids::{BaselineId, DiskId, FirmwareId, OperationId, RunId, VmId};
use crate::layout::CatalogLayout;
use crate::manifest::{self, BaselineManifest, MANIFEST_FORMAT_VERSION, ManifestFile};
use crate::paths::ManagedPath;
use crate::records::{BaselineRecord, CurrentStateRecord, DiskRecord, FirmwareRecord};
use crate::time::now_rfc3339;

/// Critical points of the baseline operations, for crash tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FaultPoint {
    /// Creation: image produced, nothing published.
    CreateBeforePublish,
    /// Creation: disk published, UEFI variables not yet.
    CreateAfterDiskPublish,
    /// Creation: files and manifest published, transaction not committed.
    CreateBeforeCommit,
    /// Restore: new overlay produced, nothing published.
    RestoreBeforePublish,
    /// Restore: new overlay and variables published, old state in place.
    RestoreAfterPublish,
    /// Restore: first file of the old state discarded (overlay discard in
    /// progress).
    RestoreAfterFirstDiscard,
    /// Restore: old state discarded, transaction not committed.
    RestoreBeforeCommit,
    /// Deletion: first file of the baseline moved away.
    DeleteAfterFirstMove,
    /// Deletion (baseline or medium): all files moved, transaction not
    /// committed.
    DeleteBeforeCommit,
    /// Any journaled operation: transaction committed, temporaries of the
    /// discarded or deleted files not yet removed.
    AfterCommit,
    /// User snapshot (ADR-0032): the caller's disk operation ended, nothing
    /// settled yet.
    SnapshotBeforeSettle,
    /// User snapshot: files published or moved, transaction not committed.
    SnapshotAfterPublish,
    /// Snapshot revert: the VM's UEFI variables removed, the snapshot's
    /// copy not yet in place.
    SnapshotRevertVarsRemoved,
    /// Snapshot revert: old TPM state removed, before the checkpoint copy is installed.
    SnapshotRevertTpmRemoved,
}

/// What happens at a [`FaultPoint`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FaultAction {
    /// Returns [`StorageError::InjectedCrash`] without any rollback, as if
    /// the process had died there (the test then drops the catalog).
    Crash,
    /// Writes the given marker file and blocks forever: a parent test kills
    /// the process (real process-kill tests).
    Pause(PathBuf),
}

impl Catalog {
    /// Arms a fault at `point` (tests only; never used by the product).
    #[doc(hidden)]
    pub fn inject_fault(&mut self, point: FaultPoint, action: FaultAction) {
        self.fault = Some((point, action));
    }

    pub(crate) fn fault_at(&self, point: FaultPoint) -> Result<()> {
        match &self.fault {
            Some((p, FaultAction::Crash)) if *p == point => Err(StorageError::InjectedCrash(point)),
            Some((p, FaultAction::Pause(marker))) if *p == point => {
                let _ = std::fs::write(marker, format!("{point:?}"));
                loop {
                    std::thread::sleep(std::time::Duration::from_secs(3600));
                }
            }
            _ => Ok(()),
        }
    }
}

/// Files of the VM's standalone working state (F2: produced by the backend,
/// not registered in the catalog), used when the VM has no current overlay.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CurrentStateFiles {
    /// Working disk (`vms/<vm>/primary.qcow2`).
    pub disk: ManagedPath,
    /// UEFI variables (`vms/<vm>/firmware/primary.vars.fd`), if any.
    pub vars: Option<ManagedPath>,
}

/// Request to create a baseline from the VM's current state.
#[derive(Debug, Clone)]
pub struct CreateBaseline {
    /// VM (must be `stopped`, no open run).
    pub vm_id: VmId,
    /// Label.
    pub label: String,
    /// Clean guest shutdown observed before the creation.
    pub clean_shutdown: bool,
    /// Protected baseline (cannot be deleted).
    pub protected: bool,
    /// Standalone working state of the VM (used without a current overlay).
    pub standalone: CurrentStateFiles,
}

/// Result of the file work of a baseline creation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProducedImage {
    /// Virtual size of the produced disk.
    pub virtual_size_bytes: u64,
    /// SHA-256 of the produced disk file.
    pub disk_sha256: String,
    /// Bytes of the produced disk file.
    pub disk_bytes: u64,
    /// SHA-256 and bytes of the copied UEFI variables.
    pub vars: Option<(String, u64)>,
    /// TPM permanent state, hashed through its guarded handle.
    pub tpm: Option<(String, u64)>,
}

/// Baseline creation started by [`Catalog::begin_create_baseline`]: intents
/// recorded, source files held open with `FILE_SHARE_READ` only (nobody can
/// modify them while the image is produced; a pre-existing writer made the
/// begin fail). [`Self::run`] needs no catalog.
#[derive(Debug)]
pub struct BaselineJob {
    req: CreateBaseline,
    op: OperationId,
    baseline_id: BaselineId,
    disk_id: DiskId,
    fw_id: FirmwareId,
    disk_intent: Intent,
    vars_intent: Option<Intent>,
    source_disk: PathBuf,
    source_vars: Option<(PathBuf, File)>,
    source_tpm: Option<(PathBuf, File)>,
    tpm_intent: Option<Intent>,
    tpm_fw_id: FirmwareId,
    derived_from: Option<BaselineId>,
    config_revision: u64,
    config: serde_json::Value,
    /// Internal snapshot of the source disk the image is made from (a user
    /// snapshot promoted to a baseline, ADR-0032).
    snapshot_tag: Option<String>,
    _source_disk_guard: File,
    _base_guard: Option<BaselineGuard>,
}

/// Outcome of [`BaselineJob::run`], to give to
/// [`Catalog::finish_create_baseline`] or [`Catalog::abort_baseline_job`].
#[derive(Debug)]
pub struct BaselineJobDone {
    job: BaselineJob,
    outcome: Result<ProducedImage>,
}

impl BaselineJobDone {
    /// Outcome of the file work.
    pub fn outcome(&self) -> std::result::Result<&ProducedImage, &StorageError> {
        self.outcome.as_ref()
    }
}

impl BaselineJob {
    /// Internal snapshot of the source disk to copy instead of its current
    /// state (`qemu-img convert -l snapshot.name=<tag>`), for a user
    /// snapshot promoted to a baseline.
    pub fn snapshot_tag(&self) -> Option<&str> {
        self.snapshot_tag.as_deref()
    }

    /// New baseline.
    pub fn baseline_id(&self) -> BaselineId {
        self.baseline_id
    }

    /// Source disk (absolute): the standalone disk or the current overlay
    /// (whose backing is the guarded base).
    pub fn source_disk(&self) -> &Path {
        &self.source_disk
    }

    /// Temporary file the standalone image must be written to (absolute,
    /// not existing yet).
    pub fn temp_disk(&self) -> &Path {
        &self.disk_intent.temp_path
    }

    /// Bytes of the source disk (and of its base, for an overlay): upper
    /// bound of the image to produce, for the budget check.
    pub fn source_bytes(&self) -> u64 {
        let disk = std::fs::metadata(&self.source_disk).map_or(0, |m| m.len());
        let vars = self
            .source_vars
            .as_ref()
            .and_then(|(p, _)| std::fs::metadata(p).ok())
            .map_or(0, |m| m.len());
        let tpm = self
            .source_tpm
            .as_ref()
            .and_then(|(_, f)| f.metadata().ok())
            .map_or(0, |m| m.len());
        disk.saturating_add(vars).saturating_add(tpm)
    }

    /// File work, on any thread: `produce(source, temp)` writes the
    /// standalone image (in the product `qemu-img convert` + `compare` +
    /// `check`) and returns its virtual size; the UEFI variables are copied
    /// from the guarded handle; both files are hashed. Never touches the
    /// catalog database.
    pub fn run(self, produce: impl FnOnce(&Path, &Path) -> Result<u64>) -> BaselineJobDone {
        let outcome = (|| {
            let temp = self.disk_intent.temp_path.clone();
            if exists_no_follow(&temp)? {
                return Err(StorageError::StateConflict(
                    "temporary file of the baseline already exists".into(),
                ));
            }
            let virtual_size_bytes = produce(&self.source_disk, &temp)?;
            let meta = std::fs::symlink_metadata(&temp).map_err(|e| StorageError::io(&temp, e))?;
            if crate::paths::is_link(&meta) || !meta.is_file() {
                return Err(StorageError::invalid_path(
                    self.disk_intent.temp_rel.as_str(),
                    PathRejection::Link,
                ));
            }
            let disk_sha256 = manifest::sha256_file(&temp)?;
            let vars = match (&self.source_vars, &self.vars_intent) {
                (Some((src, handle)), Some(vi)) => {
                    Some(copy_from_handle(handle, src, &vi.temp_path)?)
                }
                _ => None,
            };
            Ok(ProducedImage {
                virtual_size_bytes,
                disk_sha256,
                disk_bytes: meta.len(),
                vars,
                tpm: match (&self.source_tpm, &self.tpm_intent) {
                    (Some((src, handle)), Some(i)) => {
                        Some(copy_from_handle(handle, src, &i.temp_path)?)
                    }
                    _ => None,
                },
            })
        })();
        BaselineJobDone { job: self, outcome }
    }
}

/// Copies a guarded file into a new temporary (`CREATE_NEW`), syncs it and
/// returns SHA-256 and bytes of what was written.
pub(crate) fn copy_from_handle(
    mut src: &File,
    src_path: &Path,
    dst: &Path,
) -> Result<(String, u64)> {
    use sha2::{Digest, Sha256};
    src.seek(SeekFrom::Start(0))
        .map_err(|e| StorageError::io(src_path, e))?;
    let mut out = fsx::create_new_exclusive(dst)?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1 << 16];
    let mut total = 0u64;
    loop {
        let n = src
            .read(&mut buf)
            .map_err(|e| StorageError::io(src_path, e))?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
        out.write_all(&buf[..n])
            .map_err(|e| StorageError::io(dst, e))?;
        total = total.saturating_add(n as u64);
    }
    out.sync_all().map_err(|e| StorageError::io(dst, e))?;
    Ok((crate::hex(&h.finalize()), total))
}

/// Request to restore a baseline.
#[derive(Debug, Clone)]
pub struct RestoreBaseline {
    /// VM (must be `stopped`, no open run).
    pub vm_id: VmId,
    /// Baseline of the VM.
    pub baseline_id: BaselineId,
    /// Standalone working state of the VM, discarded if the VM has no
    /// current overlay yet.
    pub standalone: CurrentStateFiles,
}

/// Previous current state discarded by a restore.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OldState {
    Standalone,
    Overlay {
        disk: DiskId,
        vars: Option<FirmwareId>,
    },
}

/// Restore started by [`Catalog::begin_restore`]: intents recorded,
/// baseline guarded (`FILE_SHARE_READ` only). [`Self::run`] needs no catalog.
#[derive(Debug)]
pub struct RestoreJob {
    vm_id: VmId,
    baseline_id: BaselineId,
    overlay_id: DiskId,
    fw_id: FirmwareId,
    base_disk_id: DiskId,
    base_vars: Option<(FirmwareId, ManagedPath, PathBuf, Option<String>)>,
    base_disk_abs: PathBuf,
    virtual_size: u64,
    backing_ref: String,
    overlay_intent: Intent,
    vars_intent: Option<Intent>,
    discards: Vec<Intent>,
    old: OldState,
    guard: BaselineGuard,
    base_tpm: Option<FirmwareRecord>,
    tpm_intent: Option<Intent>,
    tpm_fw_id: FirmwareId,
    old_tpm_fw_id: Option<FirmwareId>,
}

/// Outcome of [`RestoreJob::run`].
#[derive(Debug)]
pub struct RestoreJobDone {
    job: RestoreJob,
    outcome: Result<(u64, Option<(String, u64)>)>,
}

impl RestoreJobDone {
    /// `Err` if the file work failed.
    pub fn outcome_error(&self) -> Option<&StorageError> {
        self.outcome.as_ref().err()
    }
}

impl RestoreJob {
    /// Baseline being restored.
    pub fn baseline_id(&self) -> BaselineId {
        self.baseline_id
    }

    /// Base disk of the baseline (absolute).
    pub fn base_disk(&self) -> &Path {
        &self.base_disk_abs
    }

    /// File work, on any thread: full SHA-256 verification of the baseline
    /// through the guard handles, `create(backing_ref, virtual_size, temp)`
    /// writes the new overlay (in the product `qemu-img create -u -b
    /// <backing_ref> -F qcow2`), whose header is then checked (exactly that
    /// backing reference, format `qcow2`); the baseline's variables are
    /// copied from the guarded handle.
    pub fn run(mut self, create: impl FnOnce(&str, u64, &Path) -> Result<()>) -> RestoreJobDone {
        let outcome = (|| {
            self.guard.verify()?;
            let temp = self.overlay_intent.temp_path.clone();
            if exists_no_follow(&temp)? {
                return Err(StorageError::StateConflict(
                    "temporary overlay already exists".into(),
                ));
            }
            create(&self.backing_ref, self.virtual_size, &temp)?;
            match qcow2_backing(&temp)? {
                Some((r, fmt)) if r == self.backing_ref && fmt.as_deref() == Some("qcow2") => {}
                other => {
                    return Err(StorageError::IntegrityMismatch {
                        path: self.overlay_intent.temp_rel.to_string(),
                        detail: format!("unexpected backing reference {other:?}"),
                    });
                }
            }
            let len = std::fs::metadata(&temp)
                .map_err(|e| StorageError::io(&temp, e))?
                .len();
            let vars = match (&self.base_vars, &self.vars_intent) {
                (Some((_, rel, abs, _)), Some(vi)) => {
                    let h = self.guard.handle(rel).ok_or_else(|| {
                        StorageError::StateConflict("baseline variables not guarded".into())
                    })?;
                    Some(copy_from_handle(h, abs, &vi.temp_path)?)
                }
                _ => None,
            };
            if let (Some(tpm), Some(i)) = (&self.base_tpm, &self.tpm_intent) {
                let h = self
                    .guard
                    .handle(&tpm.managed_path)
                    .ok_or_else(|| StorageError::StateConflict("TPM state not guarded".into()))?;
                copy_from_handle(h, &self.base_disk_abs, &i.temp_path)?;
            }
            Ok((len, vars))
        })();
        RestoreJobDone { job: self, outcome }
    }
}

/// Files of the current state for a start (F3), with the run guard: the
/// baseline's hash was verified through the guard handles, the overlay and
/// its variables cannot be renamed or deleted while the guard is alive.
#[derive(Debug)]
pub struct StartFiles {
    /// Baseline the overlay is backed by.
    pub baseline_id: BaselineId,
    /// Overlay (absolute).
    pub overlay: PathBuf,
    /// UEFI variables of the current state (absolute).
    pub vars: Option<PathBuf>,
    /// Base disk of the baseline (absolute): the explicit backing node.
    pub base: PathBuf,
    /// UEFI variables of the baseline (absolute), protected read-only.
    pub base_vars: Option<PathBuf>,
    /// TPM directory of the restored state (private, writable).
    pub tpm_dir: Option<PathBuf>,
    /// Guard to keep alive until the process ends.
    pub guard: RunGuard,
}

/// Information about a baseline for listings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BaselineInfo {
    /// Catalog row.
    pub record: BaselineRecord,
    /// Base disk.
    pub disk: DiskRecord,
    /// UEFI variables.
    pub vars: Option<FirmwareRecord>,
    /// TPM permanent state, if this is a TPM-bearing baseline.
    pub tpm: Option<FirmwareRecord>,
    /// Parent baseline, if the source was an overlay.
    pub derived_from: Option<BaselineId>,
    /// VM configuration revision at creation.
    pub config_revision: Option<u64>,
    /// The VM's current state is an overlay of this baseline.
    pub current: bool,
    /// Referenced (current state, overlay or open run): not deletable.
    pub in_use: bool,
}

/// Baseline facts of a VM for status displays.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BaselineOverview {
    /// Baseline of the current overlay, if any.
    pub current: Option<BaselineId>,
    /// Number of baselines.
    pub count: u32,
    /// Bytes of the baselines' files.
    pub bytes: u64,
    /// Bytes of the current overlay and its variables.
    pub overlay_bytes: u64,
}

impl Catalog {
    fn stopped_without_runs(&mut self, vm: &VmId, what: &str) -> Result<crate::records::VmRecord> {
        let (rec, open) = self.read(|tx| Ok((tx.vm(vm)?, tx.open_runs(vm)?)))?;
        if rec.lifecycle != Lifecycle::Stopped {
            return Err(StorageError::StateConflict(format!(
                "{what} requested with VM in state {}",
                rec.lifecycle
            )));
        }
        if !open.is_empty() {
            return Err(StorageError::StateConflict(format!(
                "{what} requested with an open run"
            )));
        }
        Ok(rec)
    }

    /// First step of a baseline creation (see the module documentation):
    /// the VM must be `stopped` without open runs; the source is the current
    /// overlay (with its base guarded too) or the standalone working disk;
    /// the source files are opened with `FILE_SHARE_READ` only (writer
    /// detector: [`StorageError::VmBusy`]); `promote_baseline` and
    /// `copy_vars` intents are recorded.
    pub fn begin_create_baseline(&mut self, req: CreateBaseline) -> Result<BaselineJob> {
        self.begin_baseline_from(req, None)
    }

    /// First step of the promotion of a user snapshot to a baseline
    /// (ADR-0032 with the rules of ADR-0005): as
    /// [`Self::begin_create_baseline`] from the standalone disk, but the
    /// image is made from the snapshot's internal state
    /// ([`BaselineJob::snapshot_tag`]) with the snapshot's copy of the UEFI
    /// variables and its configuration. Only snapshots taken with the VM
    /// off; `req.clean_shutdown` should be the snapshot's.
    pub fn begin_promote_snapshot(
        &mut self,
        req: CreateBaseline,
        snapshot: &crate::SnapshotId,
    ) -> Result<BaselineJob> {
        self.begin_baseline_from(req, Some(snapshot))
    }

    fn begin_baseline_from(
        &mut self,
        mut req: CreateBaseline,
        snapshot: Option<&crate::SnapshotId>,
    ) -> Result<BaselineJob> {
        let mut vm = self.stopped_without_runs(&req.vm_id, "baseline")?;
        let tpm_guest = crate::tpm::needs_tpm(&vm.guest_os);
        let mut snapshot_tag = None;
        // Recorded hash of the snapshot's UEFI variables (promotion only).
        let mut snapshot_vars_sha = None;
        if let Some(sid) = snapshot {
            let s = self.read(|tx| tx.snapshot(sid))?;
            if s.vm_id != req.vm_id {
                return Err(StorageError::NotFound {
                    kind: "snapshot",
                    id: sid.to_string(),
                });
            }
            if s.taken_while_running {
                return Err(StorageError::StateConflict(PROMOTE_RUNNING_SNAPSHOT.into()));
            }
            req.standalone.vars = s
                .vars_sha256
                .as_ref()
                .map(|_| CatalogLayout::snapshot_vars(&req.vm_id, sid));
            vm.config = s.config.clone();
            vm.revision = s.config_revision;
            snapshot_tag = Some(s.disk_tag);
            snapshot_vars_sha = s.vars_sha256;
        }
        let current = self.read(|tx| {
            let Some(cs) = tx.current_state(&req.vm_id)? else {
                return Ok(None);
            };
            let disk = tx.disk(&cs.overlay_disk_id)?;
            let vars = cs.vars_fw_id.as_ref().map(|f| tx.firmware(f)).transpose()?;
            Ok(Some((cs, disk, vars)))
        })?;
        if snapshot_tag.is_some() && current.is_some() {
            return Err(StorageError::StateConflict(
                crate::SNAPSHOT_ON_OVERLAY.into(),
            ));
        }
        let (disk_rel, vars_rel, derived_from, base_guard) = match current {
            Some((cs, disk, vars)) => {
                let g = self.baseline_guard(&cs.baseline_id, false)?;
                (
                    disk.managed_path,
                    vars.map(|v| v.managed_path),
                    Some(cs.baseline_id),
                    Some(g),
                )
            }
            None => (
                req.standalone.disk.clone(),
                req.standalone.vars.clone(),
                None,
                None,
            ),
        };
        let disk_abs = self.layout.resolve(&disk_rel)?;
        if !exists_no_follow(&disk_abs)? {
            return Err(StorageError::StateConflict(
                "the VM has no working disk yet: start it once before taking a baseline".into(),
            ));
        }
        let disk_guard = fsx::open_regular(&disk_abs, Share::ReadOnly, disk_rel.as_str())?;
        let source_vars = match vars_rel {
            Some(rel) => {
                let abs = self.layout.resolve(&rel)?;
                if exists_no_follow(&abs)? {
                    let f = fsx::open_regular(&abs, Share::ReadOnly, rel.as_str())?;
                    Some((abs, f))
                } else {
                    None
                }
            }
            None => None,
        };
        // A promoted snapshot's variables are checked against the hash
        // recorded when it was taken (as a revert does): through the guard
        // handle, so the copy made by the job reads the verified bytes.
        if let (Some(expected), Some(sid)) = (&snapshot_vars_sha, snapshot) {
            let rel = CatalogLayout::snapshot_vars(&req.vm_id, sid);
            let Some((abs, handle)) = &source_vars else {
                return Err(StorageError::IntegrityMismatch {
                    path: rel.to_string(),
                    detail: "the snapshot's UEFI variables are missing".into(),
                });
            };
            let (actual, _) = fsx::sha256_handle(handle, abs)?;
            if actual != *expected {
                return Err(StorageError::IntegrityMismatch {
                    path: rel.to_string(),
                    detail: "the snapshot's UEFI variables were altered".into(),
                });
            }
        }
        let source_tpm = if tpm_guest {
            Some(if let Some(sid) = snapshot {
                let expected = self.read(|tx| tx.snapshot_tpm(sid))?.ok_or_else(|| {
                    StorageError::StateConflict("snapshot has no TPM state".into())
                })?;
                let rel = CatalogLayout::snapshot_tpm(&req.vm_id, sid);
                let abs = self.layout.resolve(&rel)?;
                let handle = fsx::open_regular(&abs, Share::ReadOnly, rel.as_str())?;
                let actual = fsx::sha256_handle(&handle, &abs)?;
                if actual != expected {
                    return Err(StorageError::IntegrityMismatch {
                        path: rel.to_string(),
                        detail: "snapshot TPM state changed".into(),
                    });
                }
                (abs, handle)
            } else {
                let dir = self.current_tpm_dir(&req.vm_id)?;
                self.open_tpm_state(&dir)?
            })
        } else {
            None
        };
        let op = OperationId::new_v7();
        let baseline_id = BaselineId::new_v7();
        let disk_id = DiskId::new_v7();
        let fw_id = FirmwareId::new_v7();
        let disk_intent = self.begin_intent(IntentSpec {
            kind: IntentKind::PromoteBaseline,
            vm_id: Some(req.vm_id),
            operation_id: Some(op),
            target_kind: TargetKind::Disk,
            target_id: Some(disk_id.to_string()),
            final_path: Some(CatalogLayout::baseline_disk(&req.vm_id, &baseline_id)),
            read_only: true,
        })?;
        let vars_intent = match &source_vars {
            Some(_) => match self.begin_intent(IntentSpec {
                kind: IntentKind::CopyVars,
                vm_id: Some(req.vm_id),
                operation_id: Some(op),
                target_kind: TargetKind::Firmware,
                target_id: Some(fw_id.to_string()),
                final_path: Some(CatalogLayout::baseline_vars(&req.vm_id, &baseline_id)),
                read_only: true,
            }) {
                Ok(i) => Some(i),
                Err(e) => {
                    let _ = self.abort(&disk_intent);
                    return Err(e);
                }
            },
            None => None,
        };
        let tpm_fw_id = FirmwareId::new_v7();
        let tpm_intent = if source_tpm.is_some() {
            match self.begin_intent(IntentSpec {
                kind: IntentKind::CopyVars,
                vm_id: Some(req.vm_id),
                operation_id: Some(op),
                target_kind: TargetKind::Firmware,
                target_id: Some(tpm_fw_id.to_string()),
                final_path: Some(CatalogLayout::baseline_tpm(&req.vm_id, &baseline_id)),
                read_only: true,
            }) {
                Ok(i) => Some(i),
                Err(e) => {
                    self.abort_intents(
                        &[Some(disk_intent.clone()), vars_intent.clone()]
                            .into_iter()
                            .flatten()
                            .collect::<Vec<_>>(),
                    );
                    return Err(e);
                }
            }
        } else {
            None
        };
        Ok(BaselineJob {
            source_tpm,
            tpm_intent,
            tpm_fw_id,
            req,
            op,
            baseline_id,
            disk_id,
            fw_id,
            disk_intent,
            vars_intent,
            source_disk: disk_abs,
            source_vars,
            derived_from,
            config_revision: vm.revision,
            config: vm.config,
            snapshot_tag,
            _source_disk_guard: disk_guard,
            _base_guard: base_guard,
        })
    }

    /// Last step of a baseline creation: publishes the image and the
    /// variables read-only, writes the manifest and registers disk, firmware
    /// and baseline in one transaction. Any error rolls everything back.
    pub fn finish_create_baseline(&mut self, done: BaselineJobDone) -> Result<BaselineRecord> {
        let BaselineJobDone { job, outcome } = done;
        let mut intents = vec![job.disk_intent.clone()];
        intents.extend(job.vars_intent.clone());
        intents.extend(job.tpm_intent.clone());
        let result = (|| -> Result<BaselineRecord> {
            let produced = outcome?;
            self.fault_at(FaultPoint::CreateBeforePublish)?;
            self.mark_applied(&job.disk_intent)?;
            let disk_rel = CatalogLayout::baseline_disk(&job.req.vm_id, &job.baseline_id);
            let disk_abs = self.layout.resolve(&disk_rel)?;
            let published = std::fs::metadata(&disk_abs)
                .map_err(|e| StorageError::io(&disk_abs, e))?
                .len();
            if published != produced.disk_bytes {
                return Err(StorageError::IntegrityMismatch {
                    path: disk_rel.to_string(),
                    detail: format!(
                        "{published} bytes published, {} produced",
                        produced.disk_bytes
                    ),
                });
            }
            self.fault_at(FaultPoint::CreateAfterDiskPublish)?;
            if let Some(vi) = &job.vars_intent {
                self.mark_applied(vi)?;
            }
            if let Some(ti) = &job.tpm_intent {
                self.mark_applied(ti)?;
            }
            let tpm_rel = job
                .tpm_intent
                .as_ref()
                .map(|_| CatalogLayout::baseline_tpm(&job.req.vm_id, &job.baseline_id));
            let vars_rel = job
                .vars_intent
                .as_ref()
                .map(|_| CatalogLayout::baseline_vars(&job.req.vm_id, &job.baseline_id));
            let created_at = now_rfc3339();
            let m = BaselineManifest {
                format_version: MANIFEST_FORMAT_VERSION,
                kind: "baseline".into(),
                baseline_id: job.baseline_id,
                vm_id: job.req.vm_id,
                label: job.req.label.clone(),
                config_revision: job.config_revision,
                config: job.config.clone(),
                disk: ManifestFile {
                    role: "base".into(),
                    path: disk_rel.to_string(),
                    sha256: Some(produced.disk_sha256.clone()),
                },
                vars: match (&vars_rel, &produced.vars) {
                    (Some(rel), Some((sha, _))) => Some(ManifestFile {
                        role: "vars".into(),
                        path: rel.to_string(),
                        sha256: Some(sha.clone()),
                    }),
                    _ => None,
                },
                tpm: match (&tpm_rel, &produced.tpm) {
                    (Some(rel), Some((sha, _))) => Some(ManifestFile {
                        role: "tpm".into(),
                        path: rel.to_string(),
                        sha256: Some(sha.clone()),
                    }),
                    _ => None,
                },
                created_powered_off: true,
                clean_shutdown: job.req.clean_shutdown,
                created_at: created_at.clone(),
            };
            let manifest_sha = manifest::write_atomic(
                &self.layout,
                &CatalogLayout::baseline_manifest(&job.req.vm_id, &job.baseline_id),
                &m,
            )?;
            self.fault_at(FaultPoint::CreateBeforeCommit)?;
            let record = BaselineRecord {
                baseline_id: job.baseline_id,
                vm_id: job.req.vm_id,
                base_disk_id: job.disk_id,
                vars_fw_id: vars_rel.as_ref().map(|_| job.fw_id),
                label: job.req.label.clone(),
                manifest_sha256: manifest_sha,
                clean_shutdown: job.req.clean_shutdown,
                protected: job.req.protected,
                created_at: created_at.clone(),
            };
            let derived = job.derived_from;
            let rev = job.config_revision;
            self.commit_many(&intents, |tx| {
                tx.insert_disk(&DiskRecord {
                    disk_id: job.disk_id,
                    vm_id: job.req.vm_id,
                    role: DiskRole::Base,
                    backing_disk_id: None,
                    virtual_size_bytes: produced.virtual_size_bytes,
                    allocated_bytes: produced.disk_bytes,
                    managed_path: disk_rel.clone(),
                    sha256: Some(produced.disk_sha256.clone()),
                    status: DiskStatus::Ready,
                    created_at: created_at.clone(),
                })?;
                if let (Some(rel), Some((sha, len))) = (&vars_rel, &produced.vars) {
                    tx.insert_firmware(&FirmwareRecord {
                        fw_id: job.fw_id,
                        vm_id: job.req.vm_id,
                        scope: FirmwareScope::Baseline,
                        source_fw_id: None,
                        managed_path: rel.clone(),
                        sha256: Some(sha.clone()),
                        size_bytes: *len,
                        created_at: created_at.clone(),
                    })?;
                }
                tx.insert_baseline(&record)?;
                if let (Some(rel), Some((sha, len))) = (&tpm_rel, &produced.tpm) {
                    tx.insert_firmware(&FirmwareRecord {
                        fw_id: job.tpm_fw_id,
                        vm_id: job.req.vm_id,
                        scope: FirmwareScope::Baseline,
                        source_fw_id: None,
                        managed_path: rel.clone(),
                        sha256: Some(sha.clone()),
                        size_bytes: *len,
                        created_at: created_at.clone(),
                    })?;
                    tx.set_baseline_tpm(&record.baseline_id, &job.tpm_fw_id)?;
                }
                if let Some(p) = &derived {
                    tx.set_baseline_derived_from(&record.baseline_id, p)?;
                }
                tx.set_baseline_config_revision(&record.baseline_id, rev)?;
                Ok(())
            })?;
            Ok(record)
        })();
        let _ = job.op;
        match result {
            Ok(r) => {
                self.refresh_vm_manifest(&job.req.vm_id);
                Ok(r)
            }
            Err(e @ StorageError::InjectedCrash(_)) => Err(e),
            Err(e) => {
                self.abort_intents(&intents);
                Err(e)
            }
        }
    }

    /// Discards a baseline creation whose file work ended (cancellation,
    /// shutdown): the intents are aborted and the temporaries removed.
    pub fn abort_baseline_job(&mut self, done: BaselineJobDone) -> Result<()> {
        let mut intents = vec![done.job.disk_intent.clone()];
        intents.extend(done.job.vars_intent.clone());
        intents.extend(done.job.tpm_intent.clone());
        self.abort_intents(&intents);
        Ok(())
    }

    /// First step of a restore: the VM must be `stopped` without open runs
    /// and the baseline must belong to it; the baseline is guarded
    /// (`FILE_SHARE_READ` only, writer detector); intents are recorded for
    /// the new overlay and variables (`create_overlay`, `copy_vars`) and for
    /// the files of the previous current state (`discard_file`).
    pub fn begin_restore(&mut self, req: RestoreBaseline) -> Result<RestoreJob> {
        let vm = self.stopped_without_runs(&req.vm_id, "restore")?;
        let tpm_guest = crate::tpm::needs_tpm(&vm.guest_os);
        // ADR-0032: the user snapshots live inside the VM's own disk, which
        // a restore discards. The user deletes them (or keeps the VM) first.
        if self.read(|tx| tx.snapshot_count(&req.vm_id))? > 0 {
            return Err(StorageError::StateConflict(
                crate::RESTORE_WITH_SNAPSHOTS.into(),
            ));
        }
        let (b, base, base_vars, current) = self.read(|tx| {
            let b = tx.baseline(&req.baseline_id)?;
            let d = tx.disk(&b.base_disk_id)?;
            let v = b.vars_fw_id.as_ref().map(|f| tx.firmware(f)).transpose()?;
            let cs = match tx.current_state(&req.vm_id)? {
                Some(cs) => {
                    let od = tx.disk(&cs.overlay_disk_id)?;
                    let ov = cs.vars_fw_id.as_ref().map(|f| tx.firmware(f)).transpose()?;
                    Some((cs, od, ov))
                }
                None => None,
            };
            Ok((b, d, v, cs))
        })?;
        if b.vm_id != req.vm_id {
            return Err(StorageError::NotFound {
                kind: "baseline",
                id: req.baseline_id.to_string(),
            });
        }
        if base.status != DiskStatus::Ready {
            return Err(StorageError::StateConflict(
                "baseline disk not usable".into(),
            ));
        }
        let base_tpm = self.read(|tx| tx.baseline_tpm(&req.baseline_id))?;
        if tpm_guest != base_tpm.is_some() {
            return Err(StorageError::StateConflict(
                "baseline TPM state does not match this VM".into(),
            ));
        }
        let old_tpm_fw_id = self.read(|tx| tx.current_tpm(&req.vm_id))?.map(|f| f.fw_id);
        let old_tpm = if tpm_guest {
            let dir = self.current_tpm_dir(&req.vm_id)?;
            // Refuse an unsupported state layout before discarding anything.
            drop(self.open_tpm_state(&dir)?);
            Some(crate::tpm::state_file(&dir))
        } else {
            None
        };
        let guard = self.baseline_guard(&req.baseline_id, false)?;
        let base_disk_abs = self.layout.resolve(&base.managed_path)?;
        let base_vars = match &base_vars {
            Some(f) => Some((
                f.fw_id,
                f.managed_path.clone(),
                self.layout.resolve(&f.managed_path)?,
                f.sha256.clone(),
            )),
            None => None,
        };
        // Files of the previous state.
        let mut old_files: Vec<(ManagedPath, TargetKind, Option<String>)> = Vec::new();
        let old = match &current {
            Some((cs, od, ov)) => {
                old_files.push((
                    od.managed_path.clone(),
                    TargetKind::Disk,
                    Some(od.disk_id.to_string()),
                ));
                if let Some(v) = ov {
                    old_files.push((
                        v.managed_path.clone(),
                        TargetKind::Firmware,
                        Some(v.fw_id.to_string()),
                    ));
                }
                OldState::Overlay {
                    disk: cs.overlay_disk_id,
                    vars: cs.vars_fw_id,
                }
            }
            None => {
                for rel in std::iter::once(&req.standalone.disk).chain(req.standalone.vars.iter()) {
                    if exists_no_follow(&self.layout.resolve(rel)?)? {
                        old_files.push((rel.clone(), TargetKind::None, None));
                    }
                }
                OldState::Standalone
            }
        };
        if let Some(rel) = old_tpm {
            old_files.push((
                rel,
                if old_tpm_fw_id.is_some() {
                    TargetKind::Firmware
                } else {
                    TargetKind::None
                },
                old_tpm_fw_id.map(|id| id.to_string()),
            ));
        }
        let op = OperationId::new_v7();
        let overlay_id = DiskId::new_v7();
        let tpm_fw_id = FirmwareId::new_v7();
        let fw_id = FirmwareId::new_v7();
        let mut intents: Vec<Intent> = Vec::new();
        type RestoreIntents = (Intent, Option<Intent>, Option<Intent>, Vec<Intent>);
        let begun = (|| -> Result<RestoreIntents> {
            let oi = self.begin_intent(IntentSpec {
                kind: IntentKind::CreateOverlay,
                vm_id: Some(req.vm_id),
                operation_id: Some(op),
                target_kind: TargetKind::Disk,
                target_id: Some(overlay_id.to_string()),
                final_path: Some(CatalogLayout::state_overlay(&req.vm_id, &overlay_id)),
                read_only: false,
            })?;
            intents.push(oi.clone());
            let vi = match &base_vars {
                Some(_) => {
                    let vi = self.begin_intent(IntentSpec {
                        kind: IntentKind::CopyVars,
                        vm_id: Some(req.vm_id),
                        operation_id: Some(op),
                        target_kind: TargetKind::Firmware,
                        target_id: Some(fw_id.to_string()),
                        final_path: Some(CatalogLayout::state_vars(&req.vm_id, &fw_id)),
                        read_only: false,
                    })?;
                    intents.push(vi.clone());
                    Some(vi)
                }
                None => None,
            };
            let ti = if base_tpm.is_some() {
                let ti = self.begin_intent(IntentSpec {
                    kind: IntentKind::CopyVars,
                    vm_id: Some(req.vm_id),
                    operation_id: Some(op),
                    target_kind: TargetKind::Firmware,
                    target_id: Some(tpm_fw_id.to_string()),
                    final_path: Some(crate::tpm::state_file(&CatalogLayout::state_tpm_dir(
                        &req.vm_id,
                        &overlay_id,
                    ))),
                    read_only: false,
                })?;
                intents.push(ti.clone());
                Some(ti)
            } else {
                None
            };
            let mut discards = Vec::new();
            for (rel, tk, tid) in &old_files {
                let d = self.begin_intent(IntentSpec {
                    kind: IntentKind::DiscardFile,
                    vm_id: Some(req.vm_id),
                    operation_id: Some(op),
                    target_kind: *tk,
                    target_id: tid.clone(),
                    final_path: Some(rel.clone()),
                    read_only: false,
                })?;
                intents.push(d.clone());
                discards.push(d);
            }
            Ok((oi, vi, ti, discards))
        })();
        let (overlay_intent, vars_intent, tpm_intent, discards) = match begun {
            Ok(t) => t,
            Err(e) => {
                self.abort_intents(&intents);
                return Err(e);
            }
        };
        Ok(RestoreJob {
            base_tpm,
            tpm_intent,
            tpm_fw_id,
            old_tpm_fw_id,
            vm_id: req.vm_id,
            baseline_id: req.baseline_id,
            overlay_id,
            fw_id,
            base_disk_id: base.disk_id,
            base_vars,
            base_disk_abs,
            virtual_size: base.virtual_size_bytes,
            backing_ref: CatalogLayout::overlay_backing_ref(&req.baseline_id),
            overlay_intent,
            vars_intent,
            discards,
            old,
            guard,
        })
    }

    /// Last step of a restore: publishes the new overlay and variables,
    /// moves the files of the previous state away and, in one transaction,
    /// registers the new state, deletes the rows of the old one and commits
    /// every intent. Any error rolls everything back (the old state is put
    /// back in place).
    pub fn finish_restore(&mut self, done: RestoreJobDone) -> Result<CurrentStateRecord> {
        let RestoreJobDone { job, outcome } = done;
        let mut intents = vec![job.overlay_intent.clone()];
        intents.extend(job.vars_intent.clone());
        intents.extend(job.tpm_intent.clone());
        intents.extend(job.discards.iter().cloned());
        let result = (|| -> Result<CurrentStateRecord> {
            let (overlay_len, vars) = outcome?;
            self.fault_at(FaultPoint::RestoreBeforePublish)?;
            self.mark_applied(&job.overlay_intent)?;
            if let Some(vi) = &job.vars_intent {
                self.mark_applied(vi)?;
            }
            if let Some(ti) = &job.tpm_intent {
                self.mark_applied(ti)?;
            }
            self.fault_at(FaultPoint::RestoreAfterPublish)?;
            for (k, d) in job.discards.iter().enumerate() {
                self.mark_applied(d)?;
                if k == 0 {
                    self.fault_at(FaultPoint::RestoreAfterFirstDiscard)?;
                }
            }
            self.fault_at(FaultPoint::RestoreBeforeCommit)?;
            let now = now_rfc3339();
            let state = CurrentStateRecord {
                vm_id: job.vm_id,
                baseline_id: job.baseline_id,
                overlay_disk_id: job.overlay_id,
                vars_fw_id: job.vars_intent.as_ref().map(|_| job.fw_id),
                restored_at: now.clone(),
            };
            let overlay_rel = CatalogLayout::state_overlay(&job.vm_id, &job.overlay_id);
            let vars_rel = CatalogLayout::state_vars(&job.vm_id, &job.fw_id);
            let base_vars_id = job.base_vars.as_ref().map(|(id, _, _, _)| *id);
            let base_disk_id = job.base_disk_id;
            let virtual_size = job.virtual_size;
            let old = job.old;
            let vm_id = job.vm_id;
            let fw_id = job.fw_id;
            let overlay_id = job.overlay_id;
            self.commit_many(&intents, |tx| {
                tx.insert_disk(&DiskRecord {
                    disk_id: overlay_id,
                    vm_id,
                    role: DiskRole::Overlay,
                    backing_disk_id: Some(base_disk_id),
                    virtual_size_bytes: virtual_size,
                    allocated_bytes: overlay_len,
                    managed_path: overlay_rel.clone(),
                    sha256: None,
                    status: DiskStatus::Ready,
                    created_at: now.clone(),
                })?;
                if let Some((sha, len)) = &vars {
                    tx.insert_firmware(&FirmwareRecord {
                        fw_id,
                        vm_id,
                        scope: FirmwareScope::Vm,
                        source_fw_id: base_vars_id,
                        managed_path: vars_rel.clone(),
                        sha256: Some(sha.clone()),
                        size_bytes: *len,
                        created_at: now.clone(),
                    })?;
                }
                if let (Some(tpm), Some(ti)) = (&job.base_tpm, &job.tpm_intent) {
                    tx.insert_firmware(&FirmwareRecord {
                        fw_id: job.tpm_fw_id,
                        vm_id,
                        scope: FirmwareScope::Vm,
                        source_fw_id: Some(tpm.fw_id),
                        managed_path: ti.final_rel.clone().expect("TPM final path"),
                        // Mutable state: hash describes the baseline, not its subsequent writes.
                        sha256: None,
                        size_bytes: tpm.size_bytes,
                        created_at: now.clone(),
                    })?;
                    tx.set_current_tpm(&vm_id, &job.tpm_fw_id)?;
                    if let Some(old_tpm) = job.old_tpm_fw_id {
                        tx.delete_firmware(&old_tpm)?;
                    }
                }
                tx.set_current_state(&state)?;
                if let OldState::Overlay { disk, vars } = old {
                    tx.clear_ended_run_refs(Some(&disk), vars.as_ref())?;
                    tx.delete_disk(&disk)?;
                    if let Some(f) = vars {
                        tx.delete_firmware(&f)?;
                    }
                }
                Ok(())
            })?;
            Ok(state)
        })();
        match result {
            Ok(s) => {
                drop(job.guard);
                self.refresh_vm_manifest(&job.vm_id);
                Ok(s)
            }
            Err(e @ StorageError::InjectedCrash(_)) => Err(e),
            Err(e) => {
                self.abort_intents(&intents);
                Err(e)
            }
        }
    }

    /// Discards a restore whose file work ended (cancellation, shutdown):
    /// nothing of the previous state was touched yet.
    pub fn abort_restore_job(&mut self, done: RestoreJobDone) -> Result<()> {
        let mut intents = vec![done.job.overlay_intent.clone()];
        intents.extend(done.job.vars_intent.clone());
        intents.extend(done.job.tpm_intent.clone());
        intents.extend(done.job.discards.iter().cloned());
        self.abort_intents(&intents);
        Ok(())
    }

    /// Deletes a baseline with its files (disk, variables, manifest): the VM
    /// must be `stopped`; refused if protected
    /// ([`StorageError::Referenced`] `baseline_protected`) or in use (current
    /// overlay, overlay or open run: `baseline_referenced`). One
    /// `delete_file` intent per file (target `baseline`); reconciliation
    /// rolls the group forward once a file was moved.
    pub fn delete_baseline_files(&mut self, vm: &VmId, id: &BaselineId) -> Result<()> {
        self.stopped_without_runs(vm, "baseline deletion")?;
        let (b, files) = self.read(|tx| {
            let b = tx.baseline(id)?;
            let mut files = vec![tx.disk(&b.base_disk_id)?.managed_path];
            if let Some(f) = &b.vars_fw_id {
                files.push(tx.firmware(f)?.managed_path);
            }
            if let Some(tpm) = tx.baseline_tpm(id)? {
                files.push(tpm.managed_path);
            }
            Ok((b, files))
        })?;
        if b.vm_id != *vm {
            return Err(StorageError::NotFound {
                kind: "baseline",
                id: id.to_string(),
            });
        }
        let mut files: Vec<ManagedPath> = files
            .into_iter()
            .filter(|rel| {
                self.layout
                    .resolve(rel)
                    .and_then(|p| exists_no_follow(&p))
                    .unwrap_or(true)
            })
            .collect();
        let manifest_rel = CatalogLayout::baseline_manifest(vm, id);
        if exists_no_follow(&self.layout.resolve(&manifest_rel)?)? {
            files.push(manifest_rel);
        }
        let op = OperationId::new_v7();
        let mut intents: Vec<Intent> = Vec::new();
        let result = (|| -> Result<()> {
            for rel in &files {
                intents.push(self.begin_intent(IntentSpec {
                    kind: IntentKind::DeleteFile,
                    vm_id: Some(*vm),
                    operation_id: Some(op),
                    target_kind: TargetKind::Baseline,
                    target_id: Some(id.to_string()),
                    final_path: Some(rel.clone()),
                    read_only: false,
                })?);
            }
            for (k, i) in intents.iter().enumerate() {
                self.mark_applied(i)?;
                if k == 0 {
                    self.fault_at(FaultPoint::DeleteAfterFirstMove)?;
                }
            }
            self.fault_at(FaultPoint::DeleteBeforeCommit)?;
            if intents.is_empty() {
                self.with_tx(|tx| tx.delete_baseline(id))
            } else {
                self.commit_many(&intents, |tx| tx.delete_baseline(id))
            }
        })();
        match result {
            Ok(()) => {
                // Committed: what follows is best effort.
                if let Ok(dir) = self.layout.resolve(&CatalogLayout::baseline_dir(vm, id)) {
                    let _ = std::fs::remove_dir(dir);
                }
                self.refresh_vm_manifest(vm);
                Ok(())
            }
            Err(e @ StorageError::InjectedCrash(_)) => Err(e),
            Err(e) => {
                self.abort_intents(&intents);
                Err(e)
            }
        }
    }

    /// Aborts intents in reverse order; an intent that cannot be aborted
    /// stays open for reconciliation.
    fn abort_intents(&mut self, intents: &[Intent]) {
        for i in intents.iter().rev() {
            let _ = self.abort(i);
        }
    }

    /// Guard and files of the VM's current state for a start (F3), `None` if
    /// the VM runs on its standalone disk. The run row must reference the
    /// current overlay (the caller fills it at insertion,
    /// [`Self::current_state_run_refs`]). Verifies: the baseline's SHA-256
    /// through the guard handles (every start, ADR-0025 point 5); the overlay
    /// header names exactly [`CatalogLayout::overlay_backing_ref`] in
    /// `qcow2`; the base has no backing file (no chains).
    pub fn guard_current_state(&mut self, vm: &VmId, run: &RunId) -> Result<Option<StartFiles>> {
        self.current_state_files(vm, run, true)
    }

    /// [`Self::guard_current_state`] **without** the baseline hash: guard
    /// handles opened (writer detector, links refused), overlay and base
    /// headers checked; the caller must run [`RunGuard::verify_baseline`] on
    /// [`StartFiles::guard`] before handing the files to QEMU (the service
    /// does it on a worker thread, so its supervisor thread is not held for
    /// the hash of a multi-GiB base disk). The guard's handles stay open
    /// from before the hash until the run ends: the hashed content is the
    /// content QEMU reads.
    pub fn open_current_state(&mut self, vm: &VmId, run: &RunId) -> Result<Option<StartFiles>> {
        self.current_state_files(vm, run, false)
    }

    fn current_state_files(
        &mut self,
        vm: &VmId,
        run: &RunId,
        verify: bool,
    ) -> Result<Option<StartFiles>> {
        let found = self.read(|tx| {
            let Some(cs) = tx.current_state(vm)? else {
                return Ok(None);
            };
            let run = tx.run(run)?;
            let b = tx.baseline(&cs.baseline_id)?;
            let overlay = tx.disk(&cs.overlay_disk_id)?;
            let vars = cs.vars_fw_id.as_ref().map(|f| tx.firmware(f)).transpose()?;
            let base = tx.disk(&b.base_disk_id)?;
            let base_vars = b.vars_fw_id.as_ref().map(|f| tx.firmware(f)).transpose()?;
            Ok(Some((cs, run, overlay, vars, base, base_vars)))
        })?;
        let Some((cs, run_rec, overlay, vars, base, base_vars)) = found else {
            return Ok(None);
        };
        if run_rec.overlay_disk_id != Some(cs.overlay_disk_id)
            || run_rec.baseline_id != Some(cs.baseline_id)
            || run_rec.vars_fw_id != cs.vars_fw_id
        {
            return Err(StorageError::StateConflict(
                "the run does not reference the VM's current state".into(),
            ));
        }
        let guard = if verify {
            self.guard_run(run)?
        } else {
            self.open_run_guard(run)?
        };
        let overlay_abs = self.layout.resolve(&overlay.managed_path)?;
        let base_abs = self.layout.resolve(&base.managed_path)?;
        let expected = CatalogLayout::overlay_backing_ref(&cs.baseline_id);
        match qcow2_backing(&overlay_abs)? {
            Some((r, fmt)) if r == expected && fmt.as_deref() == Some("qcow2") => {}
            other => {
                return Err(StorageError::IntegrityMismatch {
                    path: overlay.managed_path.to_string(),
                    detail: format!("unexpected backing reference {other:?}"),
                });
            }
        }
        if let Some(other) = qcow2_backing(&base_abs)? {
            return Err(StorageError::IntegrityMismatch {
                path: base.managed_path.to_string(),
                detail: format!("the base has a backing file {other:?}"),
            });
        }
        Ok(Some(StartFiles {
            tpm_dir: if crate::tpm::needs_tpm(&self.read(|tx| tx.vm(vm))?.guest_os) {
                let dir = CatalogLayout::state_tpm_dir(vm, &cs.overlay_disk_id);
                drop(self.open_tpm_state(&dir)?);
                Some(self.layout.resolve(&dir)?)
            } else {
                None
            },
            baseline_id: cs.baseline_id,
            overlay: overlay_abs,
            vars: vars
                .map(|v| self.layout.resolve(&v.managed_path))
                .transpose()?,
            base: base_abs,
            base_vars: base_vars
                .map(|v| self.layout.resolve(&v.managed_path))
                .transpose()?,
            guard,
        }))
    }

    /// References of a new run of the VM to its current state (baseline,
    /// overlay, variables), `(None, None, None)` on the standalone disk.
    pub fn current_state_run_refs(
        &mut self,
        vm: &VmId,
    ) -> Result<(Option<BaselineId>, Option<DiskId>, Option<FirmwareId>)> {
        let cs = self.read(|tx| tx.current_state(vm))?;
        Ok(match cs {
            Some(c) => (Some(c.baseline_id), Some(c.overlay_disk_id), c.vars_fw_id),
            None => (None, None, None),
        })
    }

    /// Baselines of a VM with their files and state, oldest first.
    pub fn baseline_infos(&mut self, vm: &VmId) -> Result<Vec<BaselineInfo>> {
        self.read(|tx| {
            let cs = tx.current_state(vm)?;
            let mut out = Vec::new();
            for b in tx.baselines_of_vm(vm)? {
                let disk = tx.disk(&b.base_disk_id)?;
                let vars = b.vars_fw_id.as_ref().map(|f| tx.firmware(f)).transpose()?;
                let derived_from = tx.baseline_derived_from(&b.baseline_id)?;
                let config_revision = tx.baseline_config_revision(&b.baseline_id)?;
                let in_use = tx.baseline_referenced(&b.baseline_id)?;
                let current = cs.as_ref().is_some_and(|c| c.baseline_id == b.baseline_id);
                out.push(BaselineInfo {
                    tpm: tx.baseline_tpm(&b.baseline_id)?,
                    record: b,
                    disk,
                    vars,
                    derived_from,
                    config_revision,
                    current,
                    in_use,
                });
            }
            Ok(out)
        })
    }

    /// Baseline facts of a VM (count, bytes, current overlay).
    pub fn baseline_overview(&mut self, vm: &VmId) -> Result<BaselineOverview> {
        let layout = self.layout.clone();
        self.read(|tx| {
            let mut o = BaselineOverview::default();
            for b in tx.baselines_of_vm(vm)? {
                o.count = o.count.saturating_add(1);
                o.bytes = o
                    .bytes
                    .saturating_add(tx.disk(&b.base_disk_id)?.allocated_bytes);
                if let Some(tpm) = tx.baseline_tpm(&b.baseline_id)? {
                    o.bytes = o.bytes.saturating_add(tpm.size_bytes);
                }
                if let Some(f) = &b.vars_fw_id {
                    o.bytes = o.bytes.saturating_add(tx.firmware(f)?.size_bytes);
                }
            }
            if let Some(cs) = tx.current_state(vm)? {
                o.current = Some(cs.baseline_id);
                let d = tx.disk(&cs.overlay_disk_id)?;
                let len = layout
                    .resolve(&d.managed_path)
                    .ok()
                    .and_then(|p| std::fs::metadata(p).ok())
                    .map_or(d.allocated_bytes, |m| m.len());
                o.overlay_bytes = len;
                if let Some(tpm) = tx.current_tpm(vm)? {
                    o.overlay_bytes = o.overlay_bytes.saturating_add(tpm.size_bytes);
                }
                if let Some(f) = &cs.vars_fw_id {
                    o.overlay_bytes = o.overlay_bytes.saturating_add(tx.firmware(f)?.size_bytes);
                }
            }
            Ok(o)
        })
    }

    /// Records whether a run ended with an observed clean guest shutdown.
    pub fn record_run_exit(&mut self, run: &RunId, clean: bool) -> Result<()> {
        self.with_tx(|tx| tx.set_run_clean_exit(run, clean))
    }

    /// `true` if a baseline taken now would be "verified": the VM never ran
    /// or its last run ended with an observed clean guest shutdown.
    pub fn last_exit_clean(&mut self, vm: &VmId) -> Result<bool> {
        Ok(match self.read(|tx| tx.last_run_clean_exit(vm))? {
            None => true,
            Some(c) => c == Some(true),
        })
    }
}

/// `QFI\xfb`.
const QCOW2_MAGIC: [u8; 4] = [0x51, 0x46, 0x49, 0xfb];
/// Header extension "backing file format name".
const EXT_BACKING_FORMAT: u32 = 0xe279_2aca;
/// Largest header region read.
const MAX_HEADER: u64 = 1 << 20;

fn be32(b: &[u8], o: usize) -> Option<u32> {
    b.get(o..o + 4)
        .map(|s| u32::from_be_bytes([s[0], s[1], s[2], s[3]]))
}

fn be64(b: &[u8], o: usize) -> Option<u64> {
    b.get(o..o + 8).map(|s| {
        let mut a = [0u8; 8];
        a.copy_from_slice(s);
        u64::from_be_bytes(a)
    })
}

/// Refusal of the promotion of a snapshot taken while the VM ran (ADR-0005:
/// an unclean state never becomes a verified baseline).
pub const PROMOTE_RUNNING_SNAPSHOT: &str = "a snapshot taken while the VM was running cannot become a baseline: only snapshots taken with the VM off";

/// Backing reference of a QCOW2 image read from its header, without
/// `qemu-img`: `None` if the image has no backing file, otherwise the
/// reference and the backing format from the header extension (if
/// present). Not a QCOW2 image (wrong magic, truncated header, reference
/// outside the header region): [`StorageError::IntegrityMismatch`].
pub fn qcow2_backing(path: &Path) -> Result<Option<(String, Option<String>)>> {
    let bad = |detail: &str| StorageError::IntegrityMismatch {
        path: path.display().to_string(),
        detail: detail.to_owned(),
    };
    let f = File::open(path).map_err(|e| StorageError::io(path, e))?;
    let mut head = Vec::new();
    f.take(MAX_HEADER)
        .read_to_end(&mut head)
        .map_err(|e| StorageError::io(path, e))?;
    if head.get(0..4) != Some(&QCOW2_MAGIC[..]) {
        return Err(bad("not a QCOW2 image"));
    }
    let version = be32(&head, 4).ok_or_else(|| bad("truncated header"))?;
    let off = be64(&head, 8).ok_or_else(|| bad("truncated header"))?;
    let size = be32(&head, 16).ok_or_else(|| bad("truncated header"))?;
    if off == 0 {
        return Ok(None);
    }
    let start = usize::try_from(off).map_err(|_| bad("backing offset out of range"))?;
    let end = start
        .checked_add(usize::try_from(size).map_err(|_| bad("backing size out of range"))?)
        .ok_or_else(|| bad("backing offset out of range"))?;
    let name = head
        .get(start..end)
        .ok_or_else(|| bad("backing reference outside the header"))?;
    let name = String::from_utf8(name.to_vec()).map_err(|_| bad("backing reference not UTF-8"))?;
    // Header extensions: after the fixed header (72 bytes in v2, the
    // `header_length` field in v3).
    let mut pos = if version >= 3 {
        be32(&head, 100).ok_or_else(|| bad("truncated header"))? as usize
    } else {
        72
    };
    let mut format = None;
    while let (Some(t), Some(len)) = (be32(&head, pos), be32(&head, pos + 4)) {
        if t == 0 {
            break;
        }
        let data = pos + 8;
        let len = len as usize;
        if t == EXT_BACKING_FORMAT {
            let v = head
                .get(data..data + len)
                .ok_or_else(|| bad("truncated header extension"))?;
            format = Some(String::from_utf8_lossy(v).into_owned());
        }
        pos = data + len.div_ceil(8) * 8;
        if pos >= head.len() {
            break;
        }
    }
    Ok(Some((name, format)))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Minimal QCOW2 v3 header with a backing reference and format.
    pub(crate) fn header(backing: Option<(&str, &str)>) -> Vec<u8> {
        let mut h = vec![0u8; 4096];
        h[0..4].copy_from_slice(&QCOW2_MAGIC);
        h[4..8].copy_from_slice(&3u32.to_be_bytes());
        h[100..104].copy_from_slice(&104u32.to_be_bytes());
        if let Some((name, fmt)) = backing {
            // Extension at 104, backing name after it.
            h[104..108].copy_from_slice(&EXT_BACKING_FORMAT.to_be_bytes());
            h[108..112].copy_from_slice(&(fmt.len() as u32).to_be_bytes());
            h[112..112 + fmt.len()].copy_from_slice(fmt.as_bytes());
            let end = 112 + fmt.len().div_ceil(8) * 8;
            // End-of-extensions marker (zeros) already there.
            let off = end + 8;
            h[8..16].copy_from_slice(&(off as u64).to_be_bytes());
            h[16..20].copy_from_slice(&(name.len() as u32).to_be_bytes());
            h[off..off + name.len()].copy_from_slice(name.as_bytes());
        }
        h
    }

    #[test]
    fn qcow2_backing_reference_is_read_from_the_header() {
        let d = tempfile_dir();
        let p = d.join("o.qcow2");
        std::fs::write(&p, header(Some(("../base/x/disk.qcow2", "qcow2")))).unwrap();
        assert_eq!(
            qcow2_backing(&p).unwrap(),
            Some(("../base/x/disk.qcow2".into(), Some("qcow2".into())))
        );
        std::fs::write(&p, header(None)).unwrap();
        assert_eq!(qcow2_backing(&p).unwrap(), None);
        std::fs::write(&p, b"not an image").unwrap();
        assert!(matches!(
            qcow2_backing(&p),
            Err(StorageError::IntegrityMismatch { .. })
        ));
        let _ = std::fs::remove_dir_all(&d);
    }

    fn tempfile_dir() -> PathBuf {
        let d = std::env::temp_dir().join(format!("lightvm-qcow2-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }
}

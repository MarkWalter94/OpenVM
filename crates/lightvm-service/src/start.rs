//! Start checks off the supervisor thread (ADR-0008, ADR-0025).
//!
//! Every start re-verifies the approved media (SHA-256, size, read-only
//! attribute) and, for a VM restored from a baseline, the baseline's base
//! disk and UEFI variables. Hashing a 6.4 GB ISO takes about a minute: done
//! on the supervisor thread it stopped everything else (console input of the
//! other VMs, stops, status, events). The start is therefore served in three
//! steps, like the media import and the baseline jobs:
//!
//! 1. **begin** (supervisor thread, [`crate::engine::CoreEngine`]): the core
//!    forwards the start (`starting`, run created); every file to verify is
//!    **opened** in a protected way and the handles are kept
//!    ([`MediaGuardPort::open_for_start`], `BaselinePort::start_disks`): the
//!    media and the baseline with `FILE_SHARE_READ` only, without following a
//!    reparse point; a writer already holding a file is detected here
//!    (sharing violation â†’ the start is refused). Opening is cheap;
//! 2. **work** ([`StartJob::run`], worker thread): SHA-256 read **through
//!    those same handles**, cancellable between 1 MiB blocks, with progress
//!    ([`ProgressCell`], shown in `VmSummary::start`);
//! 3. **finish** (supervisor thread, `VmEngine::finish_start`): if the run is
//!    still the VM's current one and nothing cancelled it, the core compares
//!    the observed facts with the registry (`vm_core::media::verify_media`,
//!    through [`PrecomputedInspector`]) and QEMU is launched; the handles are
//!    handed to the backend and kept until the run is released.
//!
//! **Why the file verified is the file QEMU opens (TOCTOU).** The guard
//! handle is opened *before* the hash and closed only when the run is
//! released. While it is open (Windows share mode `FILE_SHARE_READ`, no
//! `FILE_SHARE_WRITE`, no `FILE_SHARE_DELETE`): no other open with write
//! access succeeds (the content cannot change between hash and use), the
//! file cannot be deleted or renamed (the name cannot be re-pointed to
//! another file), and its parent directories cannot be renamed either (NTFS
//! refuses to rename a directory that contains an open file not shared for
//! delete: tested in `adapters::catalog`). QEMU opens the managed path (the
//! service-chosen catalog path, never a client path) read-only, which is
//! compatible with the guard; so the bytes hashed are the bytes QEMU reads.
//! Limits, unchanged from ADR-0025: an administrator, raw volume access or a
//! memory mapping created before the guard; on non-Windows hosts there are
//! no share modes (declared limit, R2a).
//!
//! **Cancellation.** A forced stop or an Emergency Stop of the VM, the
//! revocation or closing of the connection that asked for the start, and the
//! service shutdown cancel the work (the hash stops within one block); the
//! finish step then records the start as failed without launching anything
//! and the VM is `stopped`.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};

use lightvm_proto::ipc::{StartPhase, StartProgress};
use lightvm_proto::{MediaEntry, MediaId, OperationId, PublicError, RunId, UtcTimestamp, VmId};
use vm_core::media::ObservedMediaFacts;
use vm_core::ports::MediaInspector;

use crate::engine::StartDisks;

/// Heavy check of the files of a VM's current state (F3: the baseline's hash
/// through the guard handles), run by [`StartJob::run`]. The guard keeps the
/// files protected for as long as it is alive.
pub trait StartGuard: Send {
    /// Bytes [`Self::verify`] reads (progress).
    fn verify_bytes(&self) -> u64 {
        0
    }
    /// Verification through the handles already open (worker thread).
    /// `cancel` is polled between blocks; `progress` receives the bytes read.
    fn verify(
        &mut self,
        _cancel: &dyn Fn() -> bool,
        _progress: &dyn Fn(u64),
    ) -> Result<(), PublicError> {
        Ok(())
    }
}

/// Managed copy of an approved medium, opened in a protected way for a start
/// by [`MediaGuardPort::open_for_start`]. Dropping it closes the handle.
pub trait GuardedMedia: Send + std::fmt::Debug {
    /// Size read on the handle (progress).
    fn size_bytes(&self) -> u64;
    /// Facts (hash, size, attributes) read **through the handle** (worker
    /// thread); `cancel` is polled between blocks.
    fn observe(
        &mut self,
        cancel: &dyn Fn() -> bool,
        progress: &dyn Fn(u64),
    ) -> Result<ObservedMediaFacts, PublicError>;
}

/// Opening of the approved media for a start (supervisor thread: no hash).
pub trait MediaGuardPort {
    /// Opens the managed copy of `entry` with the guard share mode.
    fn open_for_start(&mut self, entry: &MediaEntry) -> Result<Box<dyn GuardedMedia>, PublicError>;
}

/// Progress of a start's checks, shared between the worker (writer) and
/// the supervisor thread (`vm_status`, `list_vms`).
#[derive(Debug, Default)]
pub struct ProgressCell {
    phase: AtomicU8,
    verified: AtomicU64,
    total: AtomicU64,
}

impl ProgressCell {
    fn new(phase: StartPhase, total: u64) -> Self {
        let c = Self::default();
        c.set_phase(phase);
        c.total.store(total, Ordering::Relaxed);
        c
    }

    fn set_phase(&self, p: StartPhase) {
        let v = match p {
            StartPhase::VerifyingMedia => 0,
            StartPhase::VerifyingBaseline => 1,
            StartPhase::Launching => 2,
        };
        self.phase.store(v, Ordering::Relaxed);
    }

    fn add(&self, n: u64) {
        self.verified.fetch_add(n, Ordering::Relaxed);
    }

    /// Current progress.
    pub fn snapshot(&self) -> StartProgress {
        let phase = match self.phase.load(Ordering::Relaxed) {
            0 => StartPhase::VerifyingMedia,
            1 => StartPhase::VerifyingBaseline,
            _ => StartPhase::Launching,
        };
        let total = self.total.load(Ordering::Relaxed);
        StartProgress {
            phase,
            verified_bytes: self.verified.load(Ordering::Relaxed).min(total),
            total_bytes: total,
        }
    }
}

/// Why the work of a start did not complete.
#[derive(Debug)]
pub(crate) enum StartFailure {
    /// Cancelled (forced stop, Emergency Stop, revocation, shutdown).
    Cancelled,
    /// A medium could not be read or differs from the registry: the core
    /// records the failure when it compares the facts.
    Media,
    /// The baseline check failed (integrity, I/O).
    Baseline(PublicError),
}

/// Checks of a start to run on a worker thread ([`crate::engine::VmEngine::take_start_jobs`]).
pub struct StartJob {
    pub(crate) vm_id: VmId,
    pub(crate) run_id: RunId,
    pub(crate) operation_id: OperationId,
    pub(crate) media: Vec<(MediaEntry, Box<dyn GuardedMedia>)>,
    pub(crate) disks: Option<StartDisks>,
    pub(crate) progress: Arc<ProgressCell>,
    /// Raised by the engine (forced stop, Emergency Stop of the VM).
    pub(crate) cancel: Arc<AtomicBool>,
}

impl std::fmt::Debug for StartJob {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "StartJob({}, {}, media={}, baseline={})",
            self.vm_id,
            self.operation_id,
            self.media.len(),
            self.disks.is_some()
        )
    }
}

impl StartJob {
    /// Builds the job and its shared progress and cancellation cells.
    pub(crate) fn new(
        vm_id: VmId,
        run_id: RunId,
        operation_id: OperationId,
        media: Vec<(MediaEntry, Box<dyn GuardedMedia>)>,
        disks: Option<StartDisks>,
    ) -> Self {
        let media_bytes: u64 = media.iter().map(|(_, g)| g.size_bytes()).sum();
        let base_bytes = disks.as_ref().map_or(0, |d| d.guard.verify_bytes());
        let phase = if media.is_empty() && disks.is_some() {
            StartPhase::VerifyingBaseline
        } else {
            StartPhase::VerifyingMedia
        };
        Self {
            vm_id,
            run_id,
            operation_id,
            media,
            disks,
            progress: Arc::new(ProgressCell::new(
                phase,
                media_bytes.saturating_add(base_bytes),
            )),
            cancel: Arc::default(),
        }
    }

    /// VM.
    pub fn vm_id(&self) -> VmId {
        self.vm_id
    }

    /// Start operation.
    pub fn operation_id(&self) -> OperationId {
        self.operation_id
    }

    /// Runs the checks (blocking). `extra` is an additional cancellation
    /// check (the supervisor's: Emergency Stop generation, revocation of the
    /// requesting connection, shutdown).
    pub fn run(self, extra: &dyn Fn() -> bool) -> StartDone {
        let StartJob {
            vm_id,
            run_id,
            operation_id,
            media,
            mut disks,
            progress,
            cancel,
        } = self;
        let cancelled = || cancel.load(Ordering::SeqCst) || extra();
        let add = |n: u64| progress.add(n);
        let mut facts = BTreeMap::new();
        let mut guards = Vec::with_capacity(media.len());
        let mut outcome: Result<(), StartFailure> = Ok(());
        let now = UtcTimestamp::now();
        for (entry, mut g) in media {
            if outcome.is_ok() {
                progress.set_phase(StartPhase::VerifyingMedia);
                match g.observe(&cancelled, &add) {
                    Ok(f) => {
                        // Early stop only: the core compares again at the
                        // finish step, and that comparison decides.
                        let ok = vm_core::media::verify_media(&entry, &f, now).is_ok();
                        facts.insert(entry.record.id, Ok(f));
                        if !ok {
                            outcome = Err(StartFailure::Media);
                        }
                    }
                    Err(_) if cancelled() => outcome = Err(StartFailure::Cancelled),
                    Err(e) => {
                        facts.insert(entry.record.id, Err(e));
                        outcome = Err(StartFailure::Media);
                    }
                }
            }
            guards.push(g);
        }
        if outcome.is_ok()
            && let Some(d) = disks.as_mut()
        {
            progress.set_phase(StartPhase::VerifyingBaseline);
            if let Err(e) = d.guard.verify(&cancelled, &add) {
                outcome = Err(if cancelled() {
                    StartFailure::Cancelled
                } else {
                    StartFailure::Baseline(e)
                });
            }
        }
        if outcome.is_ok() && cancelled() {
            outcome = Err(StartFailure::Cancelled);
        }
        progress.set_phase(StartPhase::Launching);
        StartDone {
            vm_id,
            run_id,
            operation_id,
            facts,
            guards,
            disks,
            outcome,
        }
    }
}

/// Result of a [`StartJob`], back to the supervisor thread.
pub struct StartDone {
    pub(crate) vm_id: VmId,
    pub(crate) run_id: RunId,
    pub(crate) operation_id: OperationId,
    pub(crate) facts: BTreeMap<MediaId, Result<ObservedMediaFacts, PublicError>>,
    /// Media handles, kept until the run is released.
    pub(crate) guards: Vec<Box<dyn GuardedMedia>>,
    pub(crate) disks: Option<StartDisks>,
    pub(crate) outcome: Result<(), StartFailure>,
}

impl StartDone {
    /// VM.
    pub fn vm_id(&self) -> VmId {
        self.vm_id
    }

    /// Start operation.
    pub fn operation_id(&self) -> OperationId {
        self.operation_id
    }
}

impl std::fmt::Debug for StartDone {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "StartDone({}, {}, {:?})",
            self.vm_id, self.operation_id, self.outcome
        )
    }
}

/// Inspector answering with the facts observed by the worker, so that the
/// comparison with the registry stays in the core
/// (`vm_core::Core::execute_dispatch` â†’ `verify_media`).
#[derive(Debug, Default)]
pub(crate) struct PrecomputedInspector(
    pub(crate) BTreeMap<MediaId, Result<ObservedMediaFacts, PublicError>>,
);

impl MediaInspector for PrecomputedInspector {
    fn inspect(&mut self, entry: &MediaEntry) -> Result<ObservedMediaFacts, PublicError> {
        self.0
            .get(&entry.record.id)
            .cloned()
            .unwrap_or_else(|| Err(PublicError::infrastructure("medium not verified")))
    }
}

/// Error of a cancelled start.
pub(crate) fn cancelled_error() -> PublicError {
    PublicError::state_conflict("start cancelled during the verification of its files")
}

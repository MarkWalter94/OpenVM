//! VM state machine (ROADMAP 6.3; `docs/ARCHITECTURE.md` section 4).
//!
//! The transition table is a pure function ([`VmState::can_transition_to`]):
//! the application guards (queue, sessions, `run_id`) live in `vm-core`.
//! The state *persisted* in the database is a hint; the *observed* state
//! ([`ObservedState`]) derives from the process and QMP and takes precedence.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{RunId, UtcTimestamp};

/// Lifecycle state of a VM.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum VmState {
    /// Process absent and files released. The only state for operations on disks, NVRAM and media.
    Stopped,
    /// Spawn started, new `run_id` assigned, QMP not ready yet.
    Starting,
    /// Process alive and QMP ready.
    Running,
    /// Stop requested; complete only once the process end is observed.
    Stopping,
    /// Start error or unexpected exit. No automatic promotion to baseline.
    Failed,
    /// Reconciliation after a crash (journal, files, processes): no external mutation.
    Recovering,
}

impl VmState {
    /// All states, in declaration order.
    pub const ALL: [VmState; 6] = [
        VmState::Stopped,
        VmState::Starting,
        VmState::Running,
        VmState::Stopping,
        VmState::Failed,
        VmState::Recovering,
    ];

    /// States directly reachable from `self`. Pure function, frozen table:
    ///
    /// | From | To |
    /// | --- | --- |
    /// | `stopped` | `starting` |
    /// | `starting` | `running`, `stopping` (force-stop, Emergency Stop), `failed`, `recovering` |
    /// | `running` | `stopping`, `failed` (unexpected exit), `recovering` |
    /// | `stopping` | `stopped`, `failed`, `recovering` |
    /// | `failed` | `recovering` |
    /// | `recovering` | `stopped`, `failed` |
    ///
    /// Entering `recovering` from `starting`/`running`/`stopping` covers
    /// reconciliation after a supervisor crash, when the persisted state
    /// is no longer confirmed by the process and QMP. No reflexive transitions.
    #[must_use]
    pub const fn allowed_transitions(self) -> &'static [VmState] {
        match self {
            VmState::Stopped => &[VmState::Starting],
            VmState::Starting => &[
                VmState::Running,
                VmState::Stopping,
                VmState::Failed,
                VmState::Recovering,
            ],
            VmState::Running => &[VmState::Stopping, VmState::Failed, VmState::Recovering],
            VmState::Stopping => &[VmState::Stopped, VmState::Failed, VmState::Recovering],
            VmState::Failed => &[VmState::Recovering],
            VmState::Recovering => &[VmState::Stopped, VmState::Failed],
        }
    }

    /// `true` if the transition `self → to` is allowed by the table.
    #[must_use]
    pub fn can_transition_to(self, to: VmState) -> bool {
        self.allowed_transitions().contains(&to)
    }

    /// `true` if a QEMU process of this VM exists (or may exist).
    #[must_use]
    pub const fn has_process(self) -> bool {
        matches!(
            self,
            VmState::Starting | VmState::Running | VmState::Stopping
        )
    }

    /// Stable `snake_case` name (same as the serialization).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            VmState::Stopped => "stopped",
            VmState::Starting => "starting",
            VmState::Running => "running",
            VmState::Stopping => "stopping",
            VmState::Failed => "failed",
            VmState::Recovering => "recovering",
        }
    }
}

impl std::fmt::Display for VmState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Source of a state observation.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum ObservationSource {
    /// QEMU process handle (alive, terminated, exit code).
    Process,
    /// QMP reply or event (`query-status`, `SHUTDOWN`, ...).
    Qmp,
    /// Value persisted in the catalog: a hint, never sufficient on its own.
    Database,
}

/// Observed state of a VM, with source and instant.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ObservedState {
    /// Observed state.
    pub state: VmState,
    /// Source of the observation.
    pub source: ObservationSource,
    /// Run the observation refers to, if a process exists.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<RunId>,
    /// Instant of the observation.
    pub observed_at: UtcTimestamp,
}

impl ObservedState {
    /// `true` if the observation comes from the process or QMP, i.e. it can confirm the state.
    #[must_use]
    pub fn is_authoritative(&self) -> bool {
        !matches!(self.source, ObservationSource::Database)
    }
}

/// Elementary event observed by the backend, used by the core to derive the state.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum BackendObservation {
    /// Process created and associated with the run.
    ProcessSpawned {
        /// Run.
        run_id: RunId,
    },
    /// QMP ready (capabilities negotiated) and process alive.
    QmpReady {
        /// Run.
        run_id: RunId,
    },
    /// Process end observed through the handle (not through the PID).
    ProcessExited {
        /// Run.
        run_id: RunId,
        /// `true` if the exit had been requested (ordinary stop, force-stop, Emergency Stop).
        expected: bool,
    },
    /// Start failed before the process was ready.
    StartFailed {
        /// Run.
        run_id: RunId,
    },
}

impl BackendObservation {
    /// Run the observation refers to.
    #[must_use]
    pub fn run_id(&self) -> RunId {
        match self {
            Self::ProcessSpawned { run_id }
            | Self::QmpReady { run_id }
            | Self::ProcessExited { run_id, .. }
            | Self::StartFailed { run_id } => *run_id,
        }
    }
}

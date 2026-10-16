//! Core use cases: catalog, per-VM mutation queue, sessions, Emergency
//! Stop, audit (ROADMAP 6.3, 7.3, 9; `docs/ARCHITECTURE.md` 4–6).
//!
//! Flow of a request:
//! 1. [`Core::submit`]: check on receipt (session, allowlist, effective
//!    policy, quotas), idempotency by `operation_id`, `run_id`, state and
//!    VM occupancy; then enqueueing.
//! 2. [`Core::begin_dispatch`]: new check **right before forwarding**;
//!    if the session is revoked, expired or paused the operation is discarded without
//!    effect. Otherwise the operation becomes `forwarded` and the supervisor calls
//!    the backend without holding the core.
//! 3. [`Core::finish_dispatch`] records the outcome; [`Core::observe`] applies the
//!    process and QMP observations, which take precedence over the persisted state.
//!
//! The Emergency Stop ([`Core::emergency_stop`]) does not go through the queue: it revokes the
//! sessions, discards the queue, identifies the requests already forwarded and asks for the
//! termination of the run.
//!
//! Input ownership (F4, ROADMAP 6.4/7.3): every VM has one input owner, the
//! user (default) or one agent session, never both. Input from anyone else
//! is refused on receipt and discarded right before forwarding; every change
//! of owner (and the end of the owning session) records the run whose
//! pressed keys must be released ([`Core::take_input_releases`]).

use std::collections::{BTreeMap, VecDeque};

use lightvm_proto::ipc::InputOwner;
use lightvm_proto::{
    Actor, Admission, AuditAction, AuditEvent, AuditOutcome, BackendObservation, Caller,
    Capability, DisplayGeometry, EffectivePolicy, ErrorCode, FileIdentity, Grant, GuestOs,
    HostCapacity, InputEvent, Label, Lease, MediaEntry, MediaId, MediaKind, MediaLocation,
    MediaRecord, NetworkAudit, NetworkProfile, OperationId, OperationKind, OperationRecord,
    OperationStatus, PolicyLayer, PolicyRevision, PublicError, ResourceLimits, RevocationReport,
    RunEndReason, RunId, RunRecord, Screenshot, SessionId, SessionState, SessionToken,
    Sha256Digest, TerminationCause, UtcTimestamp, VmAction, VmConfig, VmId, VmName, VmRecord,
    VmRequest, VmState,
};

use crate::media::{check_media_kind, verify_media};
use crate::policy::{
    effective_policy, effective_resource_limits, effective_user_policy, host_layer,
};
use crate::ports::{AuditSink, CatalogStore, Clock, IdGenerator, MediaInspector, VmBackend};
use crate::resources::{GuestSupport, check_guest_host, existing_disk_limits, validate_config};
use crate::session::{SessionRegistry, SessionSpec, discard_code_for};

/// Hard limit of queued operations per VM, independent of quotas.
pub const MAX_QUEUE_LEN: usize = 256;
/// Finished operations remembered per VM for idempotency purposes.
pub const IDEMPOTENCY_WINDOW: usize = 1024;
/// Refusal of a user snapshot operation while an agent session targets the
/// VM (ADR-0032: snapshots are a manual-use feature).
pub const SNAPSHOT_AGENT_SESSION: &str =
    "an agent session is using this VM: snapshots are available again when it ends";

/// `true` if an operation of the given kind is allowed in the given state (pure guard).
///
/// | Operation | Allowed states |
/// | --- | --- |
/// | `start` | `stopped` |
/// | `stop` | `running` |
/// | `force_stop` | `starting`, `running`, `stopping` |
/// | `reset`, `screenshot`, `input` | `running` (input also `stopping` for the user: [`input_allowed_for`]) |
/// | `create`, `snapshot`, `restore`, `user_snapshot_revert` | `stopped` |
/// | `user_snapshot`, `user_snapshot_delete` | `stopped`, `running` (ADR-0032) |
/// | `emergency_stop` | any (priority path) |
#[must_use]
pub fn action_allowed_in(kind: OperationKind, state: VmState) -> bool {
    match kind {
        OperationKind::Start
        | OperationKind::Create
        | OperationKind::Snapshot
        | OperationKind::Restore
        | OperationKind::UserSnapshotRevert => state == VmState::Stopped,
        OperationKind::UserSnapshot | OperationKind::UserSnapshotDelete => {
            matches!(state, VmState::Stopped | VmState::Running)
        }
        OperationKind::Stop
        | OperationKind::Reset
        | OperationKind::Screenshot
        | OperationKind::Input => state == VmState::Running,
        OperationKind::ForceStop => state.has_process(),
        OperationKind::EmergencyStop => true,
    }
}

/// `true` if `caller` may send input in `state`. Base rule
/// [`action_allowed_in`] (`running`), plus `stopping` for the **user** only:
/// after a graceful stop request the guest may show a power-off
/// confirmation that the user answers from the console (F2). Agent
/// sessions (F4) keep the base rule. Run and geometry checks are unchanged.
#[must_use]
pub fn input_allowed_for(caller: &Caller, state: VmState) -> bool {
    action_allowed_in(OperationKind::Input, state)
        || (state == VmState::Stopping && matches!(caller, Caller::User))
}

/// Core configuration.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CoreConfig {
    /// Global resource limits.
    pub global_limits: ResourceLimits,
    /// Global policy layer (maximum capabilities and quotas).
    pub global_policy: PolicyLayer,
    /// Host capacity detected by the probe.
    pub host: HostCapacity,
}

/// State of a VM as seen by the core.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VmStatus {
    /// VM.
    pub vm_id: VmId,
    /// Name.
    pub name: VmName,
    /// Current state (derived from observations).
    pub state: VmState,
    /// Current run.
    pub run_id: Option<RunId>,
    /// Current configuration.
    pub config: VmConfig,
    /// Configuration revision.
    pub config_revision: u64,
    /// Current display geometry, if known.
    pub geometry: Option<DisplayGeometry>,
    /// Queued operations not yet forwarded.
    pub queued: usize,
    /// Forwarded operations awaiting an outcome.
    pub in_flight: Vec<OperationId>,
    /// Long storage operation in progress (F3: baseline creation
    /// `snapshot` or restore `restore`): the VM is busy until it ends.
    pub storage_operation: Option<OperationKind>,
}

/// Data for starting a run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StartPlan {
    /// VM with current configuration.
    pub vm: VmRecord,
    /// Media registry entries to verify with [`verify_media`] before use.
    pub media: Vec<MediaEntry>,
}

/// Operation ready to be forwarded to the backend.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Dispatch {
    /// Operation.
    pub operation_id: OperationId,
    /// VM.
    pub vm_id: VmId,
    /// Target run (new for `start`).
    pub run_id: RunId,
    /// Action.
    pub action: VmAction,
    /// Present only for `start`.
    pub start: Option<StartPlan>,
    /// Maximum allowed size for a screenshot.
    pub max_screenshot_bytes: u64,
}

/// Successful outcome reported by the backend.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DispatchOutcome {
    /// Operation completed.
    Done,
    /// Screenshot produced.
    Screenshot(Screenshot),
}

/// Emergency Stop plan: the supervisor terminates the run with OS primitives
/// and confirms with [`Core::observe`] (`process_exited`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EmergencyStopPlan {
    /// VM.
    pub vm_id: VmId,
    /// Run to terminate, if a process exists.
    pub run_id: Option<RunId>,
    /// Discarded queue and requests already forwarded.
    pub report: RevocationReport,
    /// Terminated sessions.
    pub terminated_sessions: Vec<SessionId>,
}

/// Outcome of a session termination.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionTermination {
    /// Session.
    pub session_id: SessionId,
    /// Cause.
    pub cause: TerminationCause,
    /// Discarded queue and already forwarded requests of the session.
    pub report: RevocationReport,
    /// Requested Emergency Stops (expiry, client loss, UI close...).
    pub emergency_stops: Vec<EmergencyStopPlan>,
}

/// Data to register an approved medium (file already checked and hashed by
/// the storage).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MediaRegistration {
    /// Kind.
    pub kind: MediaKind,
    /// SHA-256 of the file.
    pub sha256: Sha256Digest,
    /// Size in bytes.
    pub size_bytes: u64,
    /// Declared origin.
    pub declared_origin: Label,
    /// Managed copy or the user's file used in place (ADR-0029).
    pub location: MediaLocation,
    /// OS identity of the file.
    pub file_identity: Option<FileIdentity>,
}

#[derive(Clone, Debug)]
struct Pending {
    record: OperationRecord,
    caller: Caller,
    action: VmAction,
}

impl Pending {
    fn session_id(&self) -> Option<SessionId> {
        self.record.session_id
    }
}

#[derive(Clone, Debug)]
struct VmRuntime {
    record: VmRecord,
    state: VmState,
    run_id: Option<RunId>,
    geometry: Option<DisplayGeometry>,
    queue: VecDeque<Pending>,
    in_flight: Vec<Pending>,
    history: BTreeMap<OperationId, Pending>,
    history_order: VecDeque<OperationId>,
    pending_end: Option<RunEndReason>,
    /// Long storage operation in progress (F3), outside the queue.
    storage_op: Option<StorageOp>,
    /// Network mode of the current run (ADR-0033), decided at its start.
    run_network: Option<NetworkProfile>,
}

/// Storage operation in progress on a VM (baseline creation or restore):
/// it runs outside the per-VM queue (on a worker of the supervisor) and
/// keeps the VM busy until [`Core::finish_storage_operation`].
#[derive(Clone, Debug)]
struct StorageOp {
    record: OperationRecord,
    caller: Caller,
}

impl VmRuntime {
    fn new(record: VmRecord, state: VmState) -> Self {
        Self {
            record,
            state,
            run_id: None,
            geometry: None,
            queue: VecDeque::new(),
            in_flight: Vec::new(),
            history: BTreeMap::new(),
            history_order: VecDeque::new(),
            pending_end: None,
            storage_op: None,
            run_network: None,
        }
    }

    fn find(&self, id: OperationId) -> Option<&Pending> {
        self.queue
            .iter()
            .chain(self.in_flight.iter())
            .find(|p| p.record.id == id)
            .or_else(|| self.history.get(&id))
    }

    fn lifecycle_pending(&self) -> bool {
        self.queue
            .iter()
            .chain(self.in_flight.iter())
            .any(|p| p.record.kind.is_lifecycle_mutation())
    }

    fn force_stop_pending(&self) -> bool {
        self.queue
            .iter()
            .chain(self.in_flight.iter())
            .any(|p| p.record.kind == OperationKind::ForceStop)
    }

    fn remember(&mut self, p: Pending) {
        let id = p.record.id;
        self.history.insert(id, p);
        self.history_order.push_back(id);
        while self.history_order.len() > IDEMPOTENCY_WINDOW {
            if let Some(old) = self.history_order.pop_front() {
                self.history.remove(&old);
            }
        }
    }
}

fn audit_action_for(kind: OperationKind) -> AuditAction {
    match kind {
        OperationKind::Create => AuditAction::VmCreate,
        OperationKind::Start => AuditAction::VmStart,
        OperationKind::Stop | OperationKind::ForceStop => AuditAction::VmStop,
        OperationKind::Reset => AuditAction::VmReset,
        OperationKind::Screenshot => AuditAction::VmScreenshot,
        OperationKind::Input => AuditAction::VmInput,
        OperationKind::Snapshot => AuditAction::BaselineCreate,
        OperationKind::Restore => AuditAction::BaselineRestore,
        OperationKind::EmergencyStop => AuditAction::EmergencyStop,
        OperationKind::UserSnapshot => AuditAction::SnapshotTake,
        OperationKind::UserSnapshotRevert => AuditAction::SnapshotRevert,
        OperationKind::UserSnapshotDelete => AuditAction::SnapshotDelete,
    }
}

/// Refusal of a media deletion while VMs use it: their number and names (at
/// most five, then how many more), detail `reason` = `media_in_use`.
fn media_in_use(users: &[&str]) -> PublicError {
    const SHOWN: usize = 5;
    let mut names: Vec<String> = users
        .iter()
        .take(SHOWN)
        .map(|n| format!("\u{201c}{n}\u{201d}"))
        .collect();
    if users.len() > SHOWN {
        names.push(format!("{} more", users.len() - SHOWN));
    }
    let n = users.len();
    PublicError::state_conflict(format!(
        "the medium is used by {n} VM{}: {}; remove it from their settings or delete them first",
        if n == 1 { "" } else { "s" },
        names.join(", ")
    ))
    .with_detail("reason", "media_in_use")
    .with_detail("vms", n.to_string())
}

/// Draft audit event (the core assigns `seq` and `at`).
#[derive(Default)]
struct Ev {
    tools: Option<lightvm_proto::ToolAudit>,
    vm_id: Option<VmId>,
    run_id: Option<RunId>,
    operation_id: Option<OperationId>,
    session_id: Option<SessionId>,
    capability: Option<Capability>,
    error: Option<ErrorCode>,
    termination: Option<TerminationCause>,
    network: Option<NetworkAudit>,
}

/// Supervisor core, generic over the ports.
#[derive(Debug)]
pub struct Core<S, C, I, A> {
    store: S,
    clock: C,
    ids: I,
    audit: A,
    config: CoreConfig,
    vm_policies: BTreeMap<VmId, PolicyLayer>,
    revision: PolicyRevision,
    sessions: SessionRegistry,
    vms: BTreeMap<VmId, VmRuntime>,
    /// VM of every `operation_id` ever accepted. It does not shrink with the
    /// operation history: an id that left the idempotency window stays
    /// reserved and cannot be reused for another request (32 bytes
    /// per operation for the lifetime of the supervisor).
    op_index: BTreeMap<OperationId, VmId>,
    audit_seq: u64,
    audit_failures: u64,
    /// Input owner per VM; absent = the user.
    input_owners: BTreeMap<VmId, SessionId>,
    /// Runs whose pressed keys and buttons must be released (owner change).
    input_releases: Vec<(VmId, RunId)>,
    /// Guests this host can run (ADR-0007, ADR-0028); see
    /// [`Core::set_guest_support`].
    guest_support: GuestSupport,
    /// VMware imports in progress (ADR-0030): identifier reserved for the
    /// VM and its name, refused to other creations and imports meanwhile.
    importing: BTreeMap<VmId, VmName>,
}

impl<S, C, I, A> Core<S, C, I, A>
where
    S: CatalogStore,
    C: Clock,
    I: IdGenerator,
    A: AuditSink,
{
    /// New core. Call [`Core::load_catalog`] when the supervisor starts.
    pub fn new(store: S, clock: C, ids: I, audit: A, config: CoreConfig) -> Self {
        Self {
            store,
            clock,
            ids,
            audit,
            config,
            vm_policies: BTreeMap::new(),
            revision: PolicyRevision(1),
            sessions: SessionRegistry::default(),
            vms: BTreeMap::new(),
            op_index: BTreeMap::new(),
            audit_seq: 0,
            audit_failures: 0,
            input_owners: BTreeMap::new(),
            input_releases: Vec::new(),
            guest_support: GuestOs::runs_on_this_host_with,
            importing: BTreeMap::new(),
        }
    }

    // ----- accessors -------------------------------------------------------

    /// Catalog.
    pub fn store(&self) -> &S {
        &self.store
    }
    /// Audit destination.
    pub fn audit_sink(&self) -> &A {
        &self.audit
    }
    /// Clock.
    pub fn clock(&self) -> &C {
        &self.clock
    }
    /// Session registry (read-only).
    pub fn sessions(&self) -> &SessionRegistry {
        &self.sessions
    }
    pub fn current_run(&self, vm: VmId) -> Option<RunId> {
        self.vms
            .get(&vm)
            .filter(|rt| rt.state.has_process())
            .and_then(|rt| rt.run_id)
    }

    /// Effective Tools authority for a run. Once a session has targeted a
    /// run, manual permissions are not restored until the next run.
    pub fn tools_permissions(
        &self,
        vm: VmId,
        manual: bool,
    ) -> (
        lightvm_proto::CapabilitySet,
        lightvm_proto::ToolGrant,
        Option<UtcTimestamp>,
    ) {
        let empty = || (Default::default(), Default::default(), None);
        let Some(rt) = self.vms.get(&vm) else {
            return empty();
        };
        let config = &rt.record.config.tools;
        if !config.enabled {
            return empty();
        }
        let now = self.clock.now();
        let live = self.sessions.live_for_vm(vm);
        if live.len() > 1 {
            return empty();
        }
        if let Some(id) = live.first() {
            let Some(s) = self.sessions.get(*id) else {
                return empty();
            };
            if s.state != SessionState::Active || s.expiry_at(now).is_some() {
                return empty();
            }
            let effective = effective_policy(
                &self.config.global_policy,
                &self.vm_layer(Some(vm)),
                &s.grant.as_layer(),
                &host_layer(&self.config.host),
                self.revision,
            );
            let caps = effective
                .capabilities
                .into_iter()
                .filter(|c| c.is_integration())
                .collect();
            return (
                caps,
                s.grant.tools.clone(),
                Some(s.grant.expires_at.min(s.lease.expires_at)),
            );
        }
        if !manual {
            return empty();
        }
        let mut caps = lightvm_proto::CapabilitySet::new();
        if !config.shares.is_empty() {
            caps.insert(Capability::ShareRead);
        }
        if config.shares.iter().any(|s| s.writable) {
            caps.insert(Capability::ShareWrite);
        }
        for (cap, configured) in [
            (Capability::Clipboard, config.clipboard),
            (Capability::FilesTransfer, config.files_transfer),
            (Capability::UiInspect, config.ui_inspect),
            (Capability::UiAct, config.ui_act),
            (Capability::Usb, !config.usb_devices.is_empty()),
        ] {
            if configured {
                caps.insert(cap);
            }
        }
        caps.retain(|cap| self.authorize(&Caller::User, Some(vm), *cap, now).is_ok());
        (
            caps,
            lightvm_proto::ToolGrant {
                shares: config
                    .shares
                    .iter()
                    .map(|s| (s.name.clone(), s.writable))
                    .collect(),
                usb_devices: config.usb_devices.clone(),
            },
            None,
        )
    }
    /// Current policy revision.
    pub fn policy_revision(&self) -> PolicyRevision {
        self.revision
    }
    /// Audit events not recorded because of a destination error.
    pub fn audit_failures(&self) -> u64 {
        self.audit_failures
    }
    /// Configuration.
    pub fn config(&self) -> &CoreConfig {
        &self.config
    }

    /// Updates the host capacity (new probe). Increments the policy revision.
    pub fn set_host_capacity(&mut self, host: HostCapacity) {
        self.config.host = host;
        self.revision = PolicyRevision(self.revision.0 + 1);
    }

    /// Replaces the rule deciding which guests this host can run (default
    /// [`GuestOs::runs_on_this_host_with`]). Creating, modifying or starting
    /// a VM whose guest, with the architecture detected in its installation
    /// media, the rule refuses fails with `unsupported`. Fake backends
    /// and tests that exercise guests of another architecture pass
    /// [`crate::resources::any_guest`].
    pub fn set_guest_support(&mut self, supported: GuestSupport) {
        self.guest_support = supported;
    }

    /// Sets the policy of a VM. Increments the policy revision.
    pub fn set_vm_policy(&mut self, vm_id: VmId, layer: PolicyLayer) {
        self.vm_policies.insert(vm_id, layer);
        self.revision = PolicyRevision(self.revision.0 + 1);
    }

    // ----- audit -----------------------------------------------------------

    fn emit(
        &mut self,
        actor: Actor,
        action: AuditAction,
        outcome: AuditOutcome,
        ev: Ev,
    ) -> Result<(), PublicError> {
        self.audit_seq += 1;
        let event = AuditEvent {
            seq: self.audit_seq,
            at: self.clock.now(),
            actor,
            action,
            outcome,
            vm_id: ev.vm_id,
            run_id: ev.run_id,
            operation_id: ev.operation_id,
            session_id: ev.session_id,
            capability: ev.capability,
            error: ev.error,
            termination: ev.termination,
            network: ev.network,
            tools: ev.tools,
        };
        let r = self.audit.append(&event);
        if r.is_err() {
            self.audit_failures += 1;
        }
        r
    }

    /// Event after an effect that already happened: a destination error is counted, not propagated.
    fn emit_after(&mut self, actor: Actor, action: AuditAction, outcome: AuditOutcome, ev: Ev) {
        let _ = self.emit(actor, action, outcome, ev);
    }

    fn deny(
        &mut self,
        actor: Actor,
        action: AuditAction,
        mut ev: Ev,
        err: PublicError,
    ) -> PublicError {
        ev.error = Some(err.code);
        self.emit_after(actor, action, AuditOutcome::Denied, ev);
        err
    }

    /// Records an audit event decided outside the core (service-level steps
    /// of the session lifecycle: requests, denials, agent access switch).
    /// Only identifiers, outcome and code: never declared texts or secrets.
    pub fn record_event(
        &mut self,
        actor: Actor,
        action: AuditAction,
        outcome: AuditOutcome,
        vm_id: Option<VmId>,
        session_id: Option<SessionId>,
        error: Option<ErrorCode>,
    ) {
        self.emit_after(
            actor,
            action,
            outcome,
            Ev {
                vm_id,
                session_id,
                error,
                ..Ev::default()
            },
        );
    }

    // ----- input ownership (F4) -------------------------------------------

    /// Input owner of a VM.
    #[must_use]
    pub fn input_owner(&self, vm_id: VmId) -> InputOwner {
        self.input_owners
            .get(&vm_id)
            .map_or(InputOwner::User, |s| InputOwner::Agent { session_id: *s })
    }

    /// `Ok` if `caller` owns the input of the VM.
    fn check_input_owner(&self, caller: &Caller, vm_id: VmId) -> Result<(), PublicError> {
        match (caller, self.input_owners.get(&vm_id)) {
            (Caller::User, None) => Ok(()),
            (Caller::User, Some(_)) => Err(PublicError::permission_denied(
                "input controlled by an agent session: take control first",
            )
            .with_detail("reason", "input_owned_by_agent")),
            (Caller::Agent { session_id, .. }, Some(owner)) if owner == session_id => Ok(()),
            (Caller::Agent { .. }, _) => Err(PublicError::permission_denied(
                "the session does not control the input of the VM",
            )
            .with_detail("reason", "input_not_owned")),
        }
    }

    /// Changes the input owner of a VM (decided by the user in the app). An
    /// agent owner must be a live session whose grant covers the VM and
    /// `vm.input`. Queued input of the previous owner is discarded and the
    /// pressed keys of the current run are released
    /// ([`Self::take_input_releases`]). Audited (`input_control`).
    pub fn set_input_owner(
        &mut self,
        actor: Actor,
        vm_id: VmId,
        owner: InputOwner,
    ) -> Result<(), PublicError> {
        let ev = |session_id: Option<SessionId>| Ev {
            vm_id: Some(vm_id),
            session_id,
            capability: Some(Capability::VmInput),
            ..Ev::default()
        };
        let new_session = match owner {
            InputOwner::User => None,
            InputOwner::Agent { session_id } => Some(session_id),
        };
        let check = (|| {
            if !self.vms.contains_key(&vm_id) {
                return Err(PublicError::not_found("VM does not exist"));
            }
            if let Some(sid) = new_session {
                let s = self
                    .sessions
                    .get(sid)
                    .filter(|s| s.state != SessionState::Terminated)
                    .ok_or_else(|| PublicError::not_found("session not live"))?;
                if !s.grant.vm_allowlist.contains(&vm_id)
                    || !s.grant.capabilities.contains(&Capability::VmInput)
                {
                    return Err(PublicError::permission_denied(
                        "the session grant does not cover input on this VM",
                    ));
                }
            }
            Ok(())
        })();
        if let Err(err) = check {
            return Err(self.deny(actor, AuditAction::InputControl, ev(new_session), err));
        }
        let previous = self.input_owners.get(&vm_id).copied();
        if previous == new_session {
            return Ok(());
        }
        match new_session {
            Some(sid) => {
                self.input_owners.insert(vm_id, sid);
            }
            None => {
                self.input_owners.remove(&vm_id);
            }
        }
        self.after_input_owner_change(vm_id, previous);
        self.emit_after(
            actor,
            AuditAction::InputControl,
            AuditOutcome::Succeeded,
            ev(new_session.or(previous)),
        );
        Ok(())
    }

    /// Queued input of the previous owner discarded, release of the run's
    /// pressed keys recorded.
    fn after_input_owner_change(&mut self, vm_id: VmId, previous: Option<SessionId>) {
        let Some(rt) = self.vms.get_mut(&vm_id) else {
            return;
        };
        let (stale, rest): (VecDeque<Pending>, VecDeque<Pending>) = rt
            .queue
            .drain(..)
            .partition(|p| p.record.kind == OperationKind::Input && p.session_id() == previous);
        rt.queue = rest;
        if let Some(run) = rt.run_id
            && rt.state.has_process()
        {
            self.input_releases.push((vm_id, run));
        }
        for p in stale {
            self.discard(vm_id, p, ErrorCode::PermissionDenied);
        }
    }

    /// Runs whose pressed keys and buttons must be released since the last
    /// call (input owner changed or owning session ended).
    pub fn take_input_releases(&mut self) -> Vec<(VmId, RunId)> {
        std::mem::take(&mut self.input_releases)
    }

    // ----- authorization ---------------------------------------------------

    fn vm_layer(&self, vm_id: Option<VmId>) -> PolicyLayer {
        vm_id
            .and_then(|v| self.vm_policies.get(&v).cloned())
            .unwrap_or_else(PolicyLayer::permissive)
    }

    /// Pure check (no effect) of a call and resulting effective policy.
    fn authorize(
        &self,
        caller: &Caller,
        vm_id: Option<VmId>,
        capability: Capability,
        now: UtcTimestamp,
    ) -> Result<EffectivePolicy, PublicError> {
        let host = host_layer(&self.config.host);
        let vm_layer = self.vm_layer(vm_id);
        let effective = match caller {
            Caller::User => {
                effective_user_policy(&self.config.global_policy, &vm_layer, &host, self.revision)
            }
            Caller::Agent { session_id, token } => {
                self.sessions
                    .check(*session_id, token.as_ref(), vm_id, capability, now)?;
                let grant = self
                    .sessions
                    .get(*session_id)
                    .map(|s| s.grant.as_layer())
                    .unwrap_or_else(PolicyLayer::deny_all);
                effective_policy(
                    &self.config.global_policy,
                    &vm_layer,
                    &grant,
                    &host,
                    self.revision,
                )
            }
        };
        if effective.capabilities.contains(&capability) {
            Ok(effective)
        } else if !host.capabilities.contains(&capability) {
            Err(
                PublicError::unsupported("capability not available on this host")
                    .with_detail("capability", capability.as_str()),
            )
        } else {
            Err(
                PublicError::permission_denied("action not authorized by the effective policy")
                    .with_detail("capability", capability.as_str()),
            )
        }
    }

    /// Checks immediately before/after forwarding a bounded guest Tools
    /// call. UI actions obey the same input owner as mouse and keyboard.
    pub fn authorize_tools(
        &self,
        caller: &Caller,
        vm: VmId,
        run: RunId,
        capability: Capability,
    ) -> Result<(), PublicError> {
        if !capability.is_integration() {
            return Err(PublicError::invalid_argument("not a Tools capability"));
        }
        self.authorize(caller, Some(vm), capability, self.clock.now())?;
        let rt = self
            .vms
            .get(&vm)
            .ok_or_else(|| PublicError::not_found("VM does not exist"))?;
        if rt.run_id != Some(run) || rt.state != VmState::Running {
            return Err(PublicError::state_conflict(
                "Tools need the current running VM",
            ));
        }
        if !rt.record.config.tools.enabled {
            return Err(PublicError::unsupported("Tools are disabled for this VM"));
        }
        if capability == Capability::UiAct {
            self.check_input_owner(caller, vm)?;
        }
        Ok(())
    }

    /// Admits one bounded Tools operation against session operation/input quotas.
    pub fn admit_tools(
        &mut self,
        caller: &Caller,
        vm: VmId,
        run: RunId,
        capability: Capability,
    ) -> Result<(), PublicError> {
        self.authorize_tools(caller, vm, run, capability)?;
        let effective = self.authorize(caller, Some(vm), capability, self.clock.now())?;
        if let Caller::Agent { session_id, .. } = caller {
            let session = self
                .sessions
                .get_mut(*session_id)
                .ok_or_else(|| PublicError::permission_denied("session unavailable"))?;
            if session.usage.operations >= effective.quotas.max_operations
                || (capability == Capability::UiAct
                    && session.usage.input_events >= effective.quotas.max_input_events)
            {
                return Err(PublicError::resource_limit(
                    "Tools operation quota exhausted",
                ));
            }
            session.usage.operations = session.usage.operations.saturating_add(1);
            if capability == Capability::UiAct {
                session.usage.input_events = session.usage.input_events.saturating_add(1);
            }
        }
        Ok(())
    }

    fn effective_limits(&self) -> ResourceLimits {
        effective_resource_limits(&self.config.global_limits, None, &self.config.host)
    }

    // ----- catalog ---------------------------------------------------------

    /// Loads the catalog at startup. A persisted state with a process
    /// (`starting`/`running`/`stopping`) is confirmed by neither process nor QMP
    /// after a supervisor restart: the VM enters `recovering`. So does a VM
    /// persisted as `failed` (F6): its run ended with a crash or a failed
    /// start and nothing else would ever bring it back to `stopped`.
    pub fn load_catalog(&mut self) -> Result<(), PublicError> {
        for record in self.store.list_vms()? {
            let state = if record.persisted_state.has_process()
                || record.persisted_state == VmState::Failed
            {
                VmState::Recovering
            } else {
                record.persisted_state
            };
            if state != record.persisted_state {
                self.store.set_persisted_state(record.id, state)?;
            }
            self.vms.insert(record.id, VmRuntime::new(record, state));
        }
        Ok(())
    }

    /// Registers an approved medium in the media registry (user action in the app).
    pub fn register_media(&mut self, reg: MediaRegistration) -> Result<MediaRecord, PublicError> {
        if reg.size_bytes == 0 {
            return Err(PublicError::invalid_argument("empty medium"));
        }
        if reg.size_bytes > reg.kind.max_bytes() {
            return Err(
                PublicError::resource_limit("medium exceeds the maximum size")
                    .with_detail("size_bytes", reg.size_bytes.to_string())
                    .with_detail("max_bytes", reg.kind.max_bytes().to_string()),
            );
        }
        let record = MediaRecord {
            id: self.ids.media_id(),
            kind: reg.kind,
            sha256: reg.sha256,
            size_bytes: reg.size_bytes,
            declared_origin: reg.declared_origin,
            approved_at: self.clock.now(),
            detected_guest: None,
            location: match &reg.location {
                MediaLocation::Managed(_) => None,
                MediaLocation::Referenced(p) => Some(p.to_string_lossy().into_owned()),
            },
            missing: false,
        };
        let entry = MediaEntry {
            record: record.clone(),
            location: reg.location,
            file_identity: reg.file_identity,
        };
        self.store.insert_media(&entry)?;
        Ok(record)
    }

    /// Approved media of the registry (user only: agents see media IDs only
    /// through the VM configuration).
    pub fn list_media(&self, caller: &Caller) -> Result<Vec<MediaRecord>, PublicError> {
        if matches!(caller, Caller::Agent { .. }) {
            return Err(PublicError::permission_denied(
                "media registry not available to agent sessions",
            ));
        }
        self.store.list_media()
    }

    /// Imports a medium chosen by the user with the native picker of the UI
    /// (ADR-0008, ADR-0029): the catalog checks and hashes the file, which
    /// stays where it is, and records the registry row. User only; audited
    /// (`media_import`) without the path. Blocking: hashing a multi-GiB ISO
    /// takes as long as the disk needs to read it (the supervisor can split
    /// it with [`Self::begin_media_import`] and [`Self::finish_media_import`]
    /// and hash on another thread).
    pub fn import_media(
        &mut self,
        caller: &Caller,
        source: &std::path::Path,
        kind: MediaKind,
        declared_origin: Label,
    ) -> Result<MediaRecord, PublicError> {
        self.begin_media_import(caller, |_| Ok(()))?;
        self.finish_media_import(caller, |store| {
            store.import_media(source, kind, &declared_origin)
        })
    }

    /// First step of a media import: denies (and audits) agent sessions,
    /// then runs `begin` on the catalog (checks, reservation); an error of
    /// `begin` is audited as a failed import.
    pub fn begin_media_import<T>(
        &mut self,
        caller: &Caller,
        begin: impl FnOnce(&mut S) -> Result<T, PublicError>,
    ) -> Result<T, PublicError> {
        let actor = caller.actor();
        if matches!(caller, Caller::Agent { .. }) {
            let err =
                PublicError::permission_denied("media import not available to agent sessions");
            return Err(self.deny(actor, AuditAction::MediaImport, Ev::default(), err));
        }
        begin(&mut self.store).inspect_err(|err| {
            let ev = Ev {
                error: Some(err.code),
                ..Ev::default()
            };
            self.emit_after(actor, AuditAction::MediaImport, AuditOutcome::Failed, ev);
        })
    }

    /// Last step of a media import: `finish` records the registry row; the
    /// outcome is audited.
    pub fn finish_media_import(
        &mut self,
        caller: &Caller,
        finish: impl FnOnce(&mut S) -> Result<MediaEntry, PublicError>,
    ) -> Result<MediaRecord, PublicError> {
        let actor = caller.actor();
        match finish(&mut self.store) {
            Ok(entry) => {
                self.emit_after(
                    actor,
                    AuditAction::MediaImport,
                    AuditOutcome::Succeeded,
                    Ev::default(),
                );
                Ok(entry.record)
            }
            Err(err) => {
                let ev = Ev {
                    error: Some(err.code),
                    ..Ev::default()
                };
                self.emit_after(actor, AuditAction::MediaImport, AuditOutcome::Failed, ev);
                Err(err)
            }
        }
    }

    /// Deletes an imported medium from the registry: user only; refused
    /// (`state_conflict`, the VMs named in the message) while the
    /// configuration of a VM uses it as installation or data media, so no
    /// run can hold it. The catalog removes the row, and a managed copy
    /// through its journal; a file used in place is never touched (ADR-0029).
    /// Audited (`media_delete`).
    pub fn delete_media(&mut self, caller: &Caller, media_id: MediaId) -> Result<(), PublicError> {
        let actor = caller.actor();
        if matches!(caller, Caller::Agent { .. }) {
            let err =
                PublicError::permission_denied("media deletion not available to agent sessions");
            return Err(self.deny(actor, AuditAction::MediaDelete, Ev::default(), err));
        }
        let mut users: Vec<&str> = self
            .vms
            .values()
            .filter(|rt| {
                let c = &rt.record.config;
                c.install_media == Some(media_id) || c.data_media == Some(media_id)
            })
            .map(|rt| rt.record.config.name.as_str())
            .collect();
        users.sort_unstable();
        let check = match self.store.get_media(media_id) {
            Err(e) => Err(e),
            Ok(None) => Err(PublicError::not_found("medium not found in the registry")),
            Ok(Some(_)) if !users.is_empty() => Err(media_in_use(&users)),
            Ok(Some(_)) => Ok(()),
        };
        if let Err(err) = check {
            return Err(self.deny(actor, AuditAction::MediaDelete, Ev::default(), err));
        }
        let r = self.store.remove_media(media_id);
        let (outcome, error) = match &r {
            Ok(()) => (AuditOutcome::Succeeded, None),
            Err(e) => (AuditOutcome::Failed, Some(e.code)),
        };
        self.emit_after(
            actor,
            AuditAction::MediaDelete,
            outcome,
            Ev {
                error,
                ..Ev::default()
            },
        );
        r
    }

    /// Deletes a VM: user only, VM `stopped` with no queued or forwarded
    /// operation, expected revision. The catalog removes rows and files
    /// through its journal; the VM leaves the core only if that succeeds.
    /// Audited (`vm_delete`).
    pub fn delete_vm(
        &mut self,
        caller: &Caller,
        vm_id: VmId,
        expected_revision: u64,
    ) -> Result<(), PublicError> {
        let actor = caller.actor();
        let ev = || Ev {
            vm_id: Some(vm_id),
            ..Ev::default()
        };
        if matches!(caller, Caller::Agent { .. }) {
            let err = PublicError::permission_denied("VM deletion not available to agent sessions");
            return Err(self.deny(actor, AuditAction::VmDelete, ev(), err));
        }
        let check = match self.vms.get(&vm_id) {
            None => Err(PublicError::not_found("VM does not exist")),
            Some(rt) if rt.state != VmState::Stopped => Err(PublicError::state_conflict(
                "only a stopped VM can be deleted",
            )
            .with_detail("state", rt.state.as_str())),
            Some(rt)
                if !rt.queue.is_empty() || !rt.in_flight.is_empty() || rt.storage_op.is_some() =>
            {
                Err(PublicError::vm_busy("operations in progress on the VM"))
            }
            Some(rt) if rt.record.config_revision != expected_revision => Err(
                PublicError::state_conflict("configuration modified in the meantime")
                    .with_detail("revision", rt.record.config_revision.to_string()),
            ),
            Some(_) => Ok(()),
        };
        if let Err(err) = check {
            return Err(self.deny(actor, AuditAction::VmDelete, ev(), err));
        }
        if let Err(err) = self.store.remove_vm(vm_id) {
            let e = Ev {
                error: Some(err.code),
                ..ev()
            };
            self.emit_after(actor, AuditAction::VmDelete, AuditOutcome::Failed, e);
            return Err(err);
        }
        self.vms.remove(&vm_id);
        self.vm_policies.remove(&vm_id);
        self.emit_after(actor, AuditAction::VmDelete, AuditOutcome::Succeeded, ev());
        Ok(())
    }

    /// Checks a console request for the current run of a running VM. User
    /// only. A refusal is audited here (`console_open` denied); success is
    /// **not** audited yet: the caller issues the display ticket (proxy) and
    /// then records the actual outcome with [`Self::record_console_open`],
    /// so a `console_open` succeeded event always matches a usable ticket.
    pub fn open_console(&mut self, caller: &Caller, vm_id: VmId) -> Result<VmStatus, PublicError> {
        let actor = caller.actor();
        let ev = |run_id| Ev {
            vm_id: Some(vm_id),
            run_id,
            ..Ev::default()
        };
        if matches!(caller, Caller::Agent { .. }) {
            let err = PublicError::permission_denied("console not available to agent sessions");
            return Err(self.deny(actor, AuditAction::ConsoleOpen, ev(None), err));
        }
        let status = match self.vms.get(&vm_id) {
            None => Err(PublicError::not_found("VM does not exist")),
            Some(rt) if rt.state != VmState::Running || rt.run_id.is_none() => Err(
                PublicError::state_conflict("the console requires a running VM")
                    .with_detail("state", rt.state.as_str()),
            ),
            Some(rt) => Ok(Self::status_of(rt)),
        };
        match status {
            Ok(s) => Ok(s),
            Err(err) => Err(self.deny(actor, AuditAction::ConsoleOpen, ev(None), err)),
        }
    }

    /// Records the outcome of issuing a console display ticket after
    /// [`Self::open_console`] (audit `console_open` succeeded or failed with
    /// the error code; never the token).
    pub fn record_console_open(
        &mut self,
        caller: &Caller,
        vm_id: VmId,
        run_id: RunId,
        result: Result<(), ErrorCode>,
    ) {
        let (outcome, error) = match result {
            Ok(()) => (AuditOutcome::Succeeded, None),
            Err(code) => (AuditOutcome::Failed, Some(code)),
        };
        self.emit_after(
            caller.actor(),
            AuditAction::ConsoleOpen,
            outcome,
            Ev {
                vm_id: Some(vm_id),
                run_id: Some(run_id),
                error,
                ..Ev::default()
            },
        );
    }

    /// Records the revocation of a console display ticket (audit
    /// `console_revoke` with the cause in `termination`): run ended, UI
    /// connection closed or revoked, Emergency Stop, replaced by a new
    /// ticket, service shutdown.
    pub fn record_console_revoke(
        &mut self,
        actor: Actor,
        vm_id: VmId,
        run_id: RunId,
        cause: TerminationCause,
    ) {
        self.emit_after(
            actor,
            AuditAction::ConsoleRevoke,
            AuditOutcome::Succeeded,
            Ev {
                vm_id: Some(vm_id),
                run_id: Some(run_id),
                termination: Some(cause),
                ..Ev::default()
            },
        );
    }

    /// Records an automatic reset decided by the backend for a guest whose
    /// virtual CPUs stopped on an unrecoverable error (audit `vm_auto_reset`,
    /// actor `system`): `succeeded`, `failed` with the code, or `denied` with
    /// `resource_limit` when the backend gave up and left the guest stopped.
    /// The VM state does not change: the process is alive.
    pub fn record_auto_reset(
        &mut self,
        vm_id: VmId,
        run_id: RunId,
        outcome: AuditOutcome,
        error: Option<ErrorCode>,
    ) {
        self.emit_after(
            Actor::System,
            AuditAction::VmAutoReset,
            outcome,
            Ev {
                vm_id: Some(vm_id),
                run_id: Some(run_id),
                error,
                ..Ev::default()
            },
        );
    }

    /// Side-effect-free authorization check of a call (no audit, no
    /// admission): used by the supervisor to decide whether a costly
    /// preparatory step (for example re-reading the display geometry with a
    /// capture) may run before the core admits the request, which repeats
    /// the full check and audits the outcome.
    pub fn is_authorized(&self, caller: &Caller, vm_id: VmId, capability: Capability) -> bool {
        self.authorize(caller, Some(vm_id), capability, self.clock.now())
            .is_ok()
    }

    /// The host must run the guest of `config` with the architecture
    /// detected in its installation media ([`check_guest_host`]); a VM
    /// without installation media, or whose media architecture is unknown,
    /// is checked on the guest alone.
    fn check_config_host(&self, config: &VmConfig) -> Result<(), PublicError> {
        if let Some(hw) = &config.imported {
            // An imported guest is already installed: its architecture is
            // the VMware guest type's (ADR-0030).
            if !(self.guest_support)(config.guest_os, Some(hw.arch)) {
                return Err(PublicError::unsupported(
                    "this guest system and edition cannot run on this host (for example Windows Vista 64-bit stops at boot under the Windows hypervisor, ADR-0027): the VM cannot be imported",
                )
                .with_detail("field", "guest_os"));
            }
            return check_guest_host(config.guest_os, None, self.guest_support);
        }
        let arch = match config.install_media {
            Some(id) => self
                .store
                .get_media(id)?
                .and_then(|m| m.record.detected_guest)
                .and_then(|d| d.arch),
            None => None,
        };
        check_guest_host(config.guest_os, arch, self.guest_support)
    }

    fn check_config_media(&self, config: &VmConfig) -> Result<(), PublicError> {
        for (id, kind) in [
            (config.install_media, MediaKind::InstallIso),
            (config.data_media, MediaKind::DataIso),
        ] {
            if let Some(id) = id {
                let entry = self
                    .store
                    .get_media(id)?
                    .ok_or_else(|| PublicError::not_found("medium not present in the registry"))?;
                check_media_kind(&entry, kind)?;
            }
        }
        Ok(())
    }

    /// Creates a VM in the catalog. User only: no creation from MCP in the MVP.
    pub fn create_vm(
        &mut self,
        caller: &Caller,
        config: VmConfig,
    ) -> Result<VmRecord, PublicError> {
        let actor = caller.actor();
        let ev = || Ev {
            session_id: match actor {
                Actor::Agent { session_id } => Some(session_id),
                _ => None,
            },
            ..Ev::default()
        };
        if matches!(caller, Caller::Agent { .. }) {
            let err = PublicError::permission_denied("VM creation not available to agent sessions");
            return Err(self.deny(actor, AuditAction::VmCreate, ev(), err));
        }
        let importing = self
            .importing
            .values()
            .any(|n| n.as_str().eq_ignore_ascii_case(config.name.as_str()));
        let checks = if importing {
            Err(
                PublicError::state_conflict("a VM with this name is being imported")
                    .with_detail("field", "name"),
            )
        } else {
            self.check_config_host(&config)
                .and_then(|()| validate_config(&config, &self.effective_limits()))
                .and_then(|()| self.check_config_media(&config))
        };
        if let Err(err) = checks {
            return Err(self.deny(actor, AuditAction::VmCreate, ev(), err));
        }
        let record = VmRecord {
            id: self.ids.vm_id(),
            config,
            config_revision: 1,
            persisted_state: VmState::Stopped,
            created_at: self.clock.now(),
        };
        self.store.insert_vm(&record)?;
        self.vms
            .insert(record.id, VmRuntime::new(record.clone(), VmState::Stopped));
        self.emit_after(
            actor,
            AuditAction::VmCreate,
            AuditOutcome::Succeeded,
            Ev {
                vm_id: Some(record.id),
                ..Ev::default()
            },
        );
        Ok(record)
    }

    // ----- import of VMware VMs (ADR-0030) -----------------------------------

    /// Effective resource limits a new VM must fit (host capacity and
    /// global limits), for the mapping of an imported VM.
    #[must_use]
    pub fn creation_limits(&self) -> ResourceLimits {
        self.effective_limits()
    }

    /// Checks of a VM about to be imported, before anything is written:
    /// user only (`permission_denied` for agent sessions), the same
    /// configuration checks as a creation (host support, limits, imported
    /// hardware), a name no other VM has (nor another import in progress).
    /// A refusal is audited (`vm_import`, denied). Then `begin` prepares the
    /// storage for the new VM's identifier (journal intent); its failure is
    /// audited `failed`. The name stays reserved until
    /// [`Self::finish_import`] or [`Self::abort_import`].
    pub fn begin_import<T>(
        &mut self,
        caller: &Caller,
        config: &VmConfig,
        begin: impl FnOnce(&mut S, VmId) -> Result<T, PublicError>,
    ) -> Result<(VmId, T), PublicError> {
        let actor = caller.actor();
        let ev = Ev {
            session_id: agent_session(caller),
            ..Ev::default()
        };
        if matches!(caller, Caller::Agent { .. }) {
            let err = PublicError::permission_denied("VM import not available to agent sessions");
            return Err(self.deny(actor, AuditAction::VmImport, ev, err));
        }
        let name_taken = self.name_in_use(config.name.as_str());
        let checks = if config.imported.is_none() {
            Err(PublicError::invalid_argument(
                "an imported configuration needs its imported hardware",
            ))
        } else if name_taken {
            Err(
                PublicError::state_conflict("a VM with this name already exists")
                    .with_detail("field", "name"),
            )
        } else {
            self.check_config_host(config)
                .and_then(|()| validate_config(config, &self.effective_limits()))
                .and_then(|()| self.check_config_media(config))
        };
        if let Err(err) = checks {
            return Err(self.deny(actor, AuditAction::VmImport, ev, err));
        }
        let vm_id = self.ids.vm_id();
        match begin(&mut self.store, vm_id) {
            Ok(t) => {
                self.importing.insert(vm_id, config.name.clone());
                Ok((vm_id, t))
            }
            Err(e) => {
                self.record_import_failure(caller, &e);
                Err(e)
            }
        }
    }

    /// `true` if a VM or an import in progress uses `name` (any case).
    fn name_in_use(&self, name: &str) -> bool {
        self.vms
            .values()
            .any(|rt| rt.record.config.name.as_str().eq_ignore_ascii_case(name))
            || self
                .importing
                .values()
                .any(|n| n.as_str().eq_ignore_ascii_case(name))
    }

    /// Last step of an import: `finish` writes the VM row with its
    /// converted disk in the storage (one journaled commit), then the VM is
    /// registered (`stopped`) and the import audited `succeeded`. A failure
    /// of `finish` is audited `failed` and nothing is registered.
    pub fn finish_import(
        &mut self,
        caller: &Caller,
        vm_id: VmId,
        config: VmConfig,
        finish: impl FnOnce(&mut S, &VmRecord) -> Result<(), PublicError>,
    ) -> Result<VmRecord, PublicError> {
        self.importing.remove(&vm_id);
        let record = VmRecord {
            id: vm_id,
            config,
            config_revision: 1,
            persisted_state: VmState::Stopped,
            created_at: self.clock.now(),
        };
        if let Err(e) = finish(&mut self.store, &record) {
            self.record_import_failure(caller, &e);
            return Err(e);
        }
        self.vms
            .insert(record.id, VmRuntime::new(record.clone(), VmState::Stopped));
        self.emit_after(
            caller.actor(),
            AuditAction::VmImport,
            AuditOutcome::Succeeded,
            Ev {
                vm_id: Some(vm_id),
                ..Ev::default()
            },
        );
        Ok(record)
    }

    /// An import that will not complete (conversion failed, cancelled,
    /// Emergency Stop, shutdown): `abort` rolls the storage back, the name is
    /// released and the import audited `failed` with `reason`, returned.
    pub fn abort_import(
        &mut self,
        caller: &Caller,
        vm_id: VmId,
        abort: impl FnOnce(&mut S),
        reason: PublicError,
    ) -> PublicError {
        self.importing.remove(&vm_id);
        abort(&mut self.store);
        self.record_import_failure(caller, &reason);
        reason
    }

    /// Records an import that failed after its checks: audited `vm_import`
    /// failed with the error code.
    fn record_import_failure(&mut self, caller: &Caller, error: &PublicError) {
        self.emit_after(
            caller.actor(),
            AuditAction::VmImport,
            AuditOutcome::Failed,
            Ev {
                error: Some(error.code),
                ..Ev::default()
            },
        );
    }

    /// Records an import refused before its configuration was known (the
    /// `.vmx` or its disks): audited `vm_import` denied.
    pub fn record_import_denied(&mut self, caller: &Caller, error: &PublicError) {
        let ev = Ev {
            session_id: agent_session(caller),
            ..Ev::default()
        };
        let _ = self.deny(caller.actor(), AuditAction::VmImport, ev, error.clone());
    }

    /// Modifies the configuration of a stopped VM (resources and limits,
    /// ADR-0017). User only: agent sessions do not modify VMs in
    /// F1 (the MCP tools, F5, will stay within the user's limits).
    /// Rejects VMs not stopped or with operations in progress (`state_conflict`),
    /// a different revision (`state_conflict`), a reduced disk
    /// (`invalid_argument`), a change of firmware or of operating system
    /// other than between the Linux variants (`invalid_argument`: the
    /// hardware profile follows them, ADR-0027) and resources beyond the
    /// effective limits.
    pub fn update_vm_config(
        &mut self,
        caller: &Caller,
        vm_id: VmId,
        expected_revision: u64,
        config: VmConfig,
    ) -> Result<VmRecord, PublicError> {
        let actor = caller.actor();
        let ev = Ev {
            vm_id: Some(vm_id),
            session_id: match actor {
                Actor::Agent { session_id } => Some(session_id),
                _ => None,
            },
            ..Ev::default()
        };
        if matches!(caller, Caller::Agent { .. }) {
            let err =
                PublicError::permission_denied("VM modification not available to agent sessions");
            return Err(self.deny(actor, AuditAction::VmConfigure, ev, err));
        }
        let Some(rt) = self.vms.get(&vm_id) else {
            let err = PublicError::not_found("VM does not exist");
            return Err(self.deny(actor, AuditAction::VmConfigure, ev, err));
        };
        let current = rt.record.clone();
        let check = if rt.storage_op.is_some() {
            Err(PublicError::vm_busy(
                "storage operation in progress on the VM",
            ))
        } else if rt.state != VmState::Stopped || rt.lifecycle_pending() {
            Err(PublicError::state_conflict(
                "the VM must be stopped with no operations in progress",
            )
            .with_detail("state", rt.state.as_str()))
        } else if current.config_revision != expected_revision {
            Err(
                PublicError::state_conflict("configuration modified in the meantime")
                    .with_detail("revision", current.config_revision.to_string()),
            )
        } else if config.tools != current.config.tools
            && !self.sessions.live_for_vm(vm_id).is_empty()
        {
            Err(PublicError::state_conflict(
                "end the agent sessions before changing Tools settings",
            ))
        } else if config.disk_gib < current.config.disk_gib {
            Err(PublicError::invalid_argument(
                "shrinking the disk is not supported: it can only be enlarged",
            ))
        } else {
            crate::resources::check_profile_change(&current.config, &config)
                .and_then(|()| self.check_config_host(&config))
                .and_then(|()| {
                    validate_config(
                        &config,
                        &existing_disk_limits(self.effective_limits(), &current.config),
                    )
                })
                .and_then(|()| self.check_config_media(&config))
        };
        if let Err(err) = check {
            return Err(self.deny(actor, AuditAction::VmConfigure, ev, err));
        }
        let record = VmRecord {
            config,
            config_revision: current.config_revision + 1,
            ..current
        };
        self.store.update_vm(&record, expected_revision)?;
        if let Some(rt) = self.vms.get_mut(&vm_id) {
            rt.record = record.clone();
        }
        self.emit_after(actor, AuditAction::VmConfigure, AuditOutcome::Succeeded, ev);
        Ok(record)
    }

    fn status_of(rt: &VmRuntime) -> VmStatus {
        VmStatus {
            vm_id: rt.record.id,
            name: rt.record.config.name.clone(),
            state: rt.state,
            run_id: rt.run_id,
            config: rt.record.config.clone(),
            config_revision: rt.record.config_revision,
            geometry: rt.geometry,
            queued: rt.queue.len(),
            in_flight: rt.in_flight.iter().map(|p| p.record.id).collect(),
            storage_operation: rt.storage_op.as_ref().map(|o| o.record.kind),
        }
    }

    /// List of the VMs visible to the caller (filtered by the grant for agents).
    pub fn list_vms(&mut self, caller: &Caller) -> Result<Vec<VmStatus>, PublicError> {
        let now = self.clock.now();
        let session_id = agent_session(caller);
        if let Err(err) = self.authorize(caller, None, Capability::VmList, now) {
            let ev = Ev {
                session_id,
                capability: Some(Capability::VmList),
                ..Ev::default()
            };
            return Err(self.deny(caller.actor(), AuditAction::VmList, ev, err));
        }
        let allow = match caller {
            Caller::User => None,
            Caller::Agent { session_id, .. } => self
                .sessions
                .get(*session_id)
                .map(|s| s.grant.vm_allowlist.clone()),
        };
        let out: Vec<VmStatus> = self
            .vms
            .values()
            .filter(|rt| allow.as_ref().is_none_or(|a| a.contains(&rt.record.id)))
            .map(Self::status_of)
            .collect();
        // Every agent call is audited (F4); the user's reads are not.
        if session_id.is_some() {
            self.emit_after(
                caller.actor(),
                AuditAction::VmList,
                AuditOutcome::Succeeded,
                Ev {
                    session_id,
                    ..Ev::default()
                },
            );
        }
        Ok(out)
    }

    /// State of a VM, if the caller is authorized. For an agent a VM outside the
    /// allowlist gives `permission_denied` even if it does not exist (no data about other VMs).
    pub fn vm_status(&mut self, caller: &Caller, vm_id: VmId) -> Result<VmStatus, PublicError> {
        let now = self.clock.now();
        let session_id = agent_session(caller);
        if let Err(err) = self.authorize(caller, Some(vm_id), Capability::VmStatus, now) {
            let ev = Ev {
                vm_id: Some(vm_id),
                session_id,
                capability: Some(Capability::VmStatus),
                ..Ev::default()
            };
            return Err(self.deny(caller.actor(), AuditAction::VmStatus, ev, err));
        }
        let st = self
            .vms
            .get(&vm_id)
            .map(Self::status_of)
            .ok_or_else(|| PublicError::not_found("VM does not exist"))?;
        if session_id.is_some() {
            self.emit_after(
                caller.actor(),
                AuditAction::VmStatus,
                AuditOutcome::Succeeded,
                Ev {
                    vm_id: Some(vm_id),
                    session_id,
                    ..Ev::default()
                },
            );
        }
        Ok(st)
    }

    /// Operation by id: the user sees everything, an agent only its own.
    pub fn operation(
        &self,
        caller: &Caller,
        id: OperationId,
    ) -> Result<OperationRecord, PublicError> {
        let now = self.clock.now();
        let vm = self.op_index.get(&id).copied();
        if let Caller::Agent { session_id, token } = caller {
            self.sessions
                .check(*session_id, token.as_ref(), vm, Capability::VmStatus, now)?;
        }
        let found = vm
            .and_then(|v| self.vms.get(&v))
            .and_then(|rt| rt.find(id))
            .map(|p| p.record.clone());
        match (found, caller) {
            (Some(r), Caller::User) => Ok(r),
            (Some(r), Caller::Agent { session_id, .. }) if r.session_id == Some(*session_id) => {
                Ok(r)
            }
            _ => Err(PublicError::not_found("operation does not exist")),
        }
    }

    // ----- mutation queue -------------------------------------------------

    fn running_vm_count(&self, except: VmId) -> u32 {
        let n = self
            .vms
            .values()
            .filter(|rt| rt.record.id != except && rt.state.has_process())
            .count();
        u32::try_from(n).unwrap_or(u32::MAX)
    }

    fn check_start_resources(&self, vm_id: VmId) -> Result<(), PublicError> {
        let limits = self.effective_limits();
        if self.running_vm_count(vm_id) >= limits.max_running_vms {
            return Err(
                PublicError::resource_limit("maximum number of running VMs reached")
                    .with_detail("limit", limits.max_running_vms.to_string()),
            );
        }
        let rt = self
            .vms
            .get(&vm_id)
            .ok_or_else(|| PublicError::not_found("VM does not exist"))?;
        // A catalog moved from another host may hold guests this one cannot
        // run with acceleration.
        self.check_config_host(&rt.record.config)?;
        // Per-VM limits recomputed at start: host capacity may have changed
        // after creation (new probe, increased reserve). The disk already
        // exists: free space only matters to create or enlarge it.
        validate_config(
            &rt.record.config,
            &existing_disk_limits(limits, &rt.record.config),
        )?;
        // Total guest memory of the VMs with a process, plus the requested one,
        // within host memory minus the reserve. vCPUs may be
        // overcommitted across different VMs and are not summed.
        let host_memory = self.config.host.as_limits().max_memory_mib;
        let committed: u64 = self
            .vms
            .values()
            .filter(|o| o.record.id != vm_id && o.state.has_process())
            .map(|o| o.record.config.memory_mib)
            .fold(0, u64::saturating_add);
        let requested = committed.saturating_add(rt.record.config.memory_mib);
        if requested > host_memory {
            return Err(PublicError::resource_limit(
                "insufficient host memory for the running VMs",
            )
            .with_detail("field", "memory_mib_total")
            .with_detail("requested", requested.to_string())
            .with_detail("limit", host_memory.to_string()));
        }
        Ok(())
    }

    /// Accepts a request and enqueues it (check on receipt).
    pub fn submit(&mut self, caller: &Caller, req: VmRequest) -> Result<Admission, PublicError> {
        let now = self.clock.now();
        let kind = req.action.kind();
        let capability = req.action.capability();
        let actor = caller.actor();
        let session_id = match caller {
            Caller::Agent { session_id, .. } => Some(*session_id),
            Caller::User => None,
        };
        let ev = |run_id: Option<RunId>| Ev {
            vm_id: Some(req.vm_id),
            run_id,
            operation_id: Some(req.operation_id),
            session_id,
            capability: Some(capability),
            ..Ev::default()
        };
        let audit_action = audit_action_for(kind);

        // 1. Authorization (before revealing whether the VM exists).
        let effective = match self.authorize(caller, Some(req.vm_id), capability, now) {
            Ok(e) => e,
            Err(err) => return Err(self.deny(actor, audit_action, ev(req.expected_run_id), err)),
        };
        if kind == OperationKind::Input
            && self.vms.contains_key(&req.vm_id)
            && let Err(err) = self.check_input_owner(caller, req.vm_id)
        {
            return Err(self.deny(actor, audit_action, ev(req.expected_run_id), err));
        }
        match self.admit(caller, &req, &effective) {
            Ok(Some(adm)) => Ok(adm),
            Ok(None) => {
                let record = OperationRecord {
                    id: req.operation_id,
                    vm_id: req.vm_id,
                    run_id: req.expected_run_id,
                    session_id,
                    kind,
                    status: OperationStatus::Queued,
                    error: None,
                    requested_at: now,
                    updated_at: now,
                };
                // The acceptance audit precedes the effect: if it fails, nothing is enqueued.
                self.emit(
                    actor,
                    audit_action,
                    AuditOutcome::Accepted,
                    ev(req.expected_run_id),
                )?;
                self.store.upsert_operation(&record)?;
                self.consume_quota(session_id, &req.action);
                let pending = Pending {
                    record,
                    caller: caller.clone(),
                    action: req.action,
                };
                let rt = self
                    .vms
                    .get_mut(&req.vm_id)
                    .ok_or_else(|| PublicError::not_found("VM does not exist"))?;
                if kind == OperationKind::ForceStop {
                    rt.queue.push_front(pending);
                } else {
                    rt.queue.push_back(pending);
                }
                self.op_index.insert(req.operation_id, req.vm_id);
                Ok(Admission {
                    operation_id: req.operation_id,
                    status: OperationStatus::Queued,
                    duplicate: false,
                })
            }
            Err(err) => Err(self.deny(actor, audit_action, ev(req.expected_run_id), err)),
        }
    }

    /// Admission guards. `Ok(Some)` = idempotent duplicate; `Ok(None)` = to be enqueued.
    fn admit(
        &self,
        caller: &Caller,
        req: &VmRequest,
        effective: &EffectivePolicy,
    ) -> Result<Option<Admission>, PublicError> {
        let kind = req.action.kind();
        let rt = self
            .vms
            .get(&req.vm_id)
            .ok_or_else(|| PublicError::not_found("VM does not exist"))?;

        // Idempotency: same operation_id, same VM, same action, same actor.
        if let Some(vm_of_op) = self.op_index.get(&req.operation_id) {
            let same = (*vm_of_op == req.vm_id)
                .then(|| rt.find(req.operation_id))
                .flatten()
                .filter(|p| p.action == req.action && p.caller.actor() == caller.actor());
            return match same {
                Some(p) => Ok(Some(Admission {
                    operation_id: req.operation_id,
                    status: p.record.status,
                    duplicate: true,
                })),
                None => Err(PublicError::invalid_argument(
                    "operation_id already used for a different request",
                )),
            };
        }

        // Structural validation and run_id.
        let needs_run = matches!(
            kind,
            OperationKind::Reset | OperationKind::Screenshot | OperationKind::Input
        );
        if needs_run && req.expected_run_id.is_none() {
            return Err(PublicError::invalid_argument(
                "expected_run_id required for this action",
            ));
        }
        if let VmAction::Input(batch) = &req.action {
            batch.validate()?;
            if batch.vm_id != req.vm_id || Some(batch.run_id) != req.expected_run_id {
                return Err(PublicError::invalid_argument(
                    "batch vm_id or run_id differ from the request",
                ));
            }
        }
        if let Some(expected) = req.expected_run_id
            && rt.run_id != Some(expected)
        {
            return Err(PublicError::state_conflict(
                "the request refers to a run that is not current",
            )
            .with_detail("reason", "stale_run_id"));
        }
        if let VmAction::Input(batch) = &req.action {
            let has_pointer = batch
                .events
                .iter()
                .any(|e| matches!(e, InputEvent::PointerMove(_)));
            if has_pointer && rt.geometry != Some(batch.geometry) {
                return Err(PublicError::state_conflict(
                    "display geometry not current: take a screenshot",
                )
                .with_detail("reason", "stale_geometry"));
            }
        }

        // State (input: see `input_allowed_for`).
        let allowed = if kind == OperationKind::Input {
            input_allowed_for(caller, rt.state)
        } else {
            action_allowed_in(kind, rt.state)
        };
        if !allowed {
            return Err(
                PublicError::state_conflict("action not allowed in the current state")
                    .with_detail("state", rt.state.as_str()),
            );
        }

        // Occupancy: a long storage operation (F3) keeps the VM busy.
        if rt.storage_op.is_some() {
            return Err(PublicError::vm_busy(
                "storage operation in progress on the VM",
            ));
        }
        // Occupancy: one lifecycle mutation at a time.
        if kind == OperationKind::ForceStop {
            if rt.force_stop_pending() {
                return Err(PublicError::vm_busy("forced stop already in progress"));
            }
        } else if rt.lifecycle_pending()
            // The stop forwarded while `stopping` does not block the user's
            // input (state already checked above).
            && !(kind == OperationKind::Input && rt.state == VmState::Stopping)
        {
            return Err(PublicError::vm_busy(
                "another mutation is in progress on the VM",
            ));
        }

        // Queue limits and quotas.
        let quotas = effective.quotas;
        let queue_cap = usize::try_from(quotas.max_queued_operations)
            .unwrap_or(usize::MAX)
            .min(MAX_QUEUE_LEN);
        if rt.queue.len() >= queue_cap {
            return Err(PublicError::resource_limit("VM queue full"));
        }
        if let Caller::Agent { session_id, .. } = caller
            && let Some(s) = self.sessions.get(*session_id)
        {
            let u = s.usage;
            let over = |used: u32, add: u32, max: u32| used.saturating_add(add) > max;
            if over(u.operations, 1, quotas.max_operations) {
                return Err(PublicError::resource_limit("operation quota exhausted"));
            }
            if let VmAction::Input(batch) = &req.action {
                let n = u32::try_from(batch.events.len()).unwrap_or(u32::MAX);
                if over(u.input_events, n, quotas.max_input_events) {
                    return Err(PublicError::resource_limit("input quota exhausted"));
                }
            }
            if kind == OperationKind::Screenshot && over(u.screenshots, 1, quotas.max_screenshots) {
                return Err(PublicError::resource_limit("screenshot quota exhausted"));
            }
        }
        if kind == OperationKind::Start {
            self.check_start_resources(req.vm_id)?;
        }
        Ok(None)
    }

    fn consume_quota(&mut self, session_id: Option<SessionId>, action: &VmAction) {
        let Some(s) = session_id.and_then(|id| self.sessions.get_mut(id)) else {
            return;
        };
        s.usage.operations = s.usage.operations.saturating_add(1);
        match action {
            VmAction::Input(b) => {
                let n = u32::try_from(b.events.len()).unwrap_or(u32::MAX);
                s.usage.input_events = s.usage.input_events.saturating_add(n);
            }
            VmAction::Screenshot => s.usage.screenshots = s.usage.screenshots.saturating_add(1),
            _ => {}
        }
    }

    fn discard(&mut self, vm_id: VmId, mut p: Pending, code: ErrorCode) -> OperationId {
        let now = self.clock.now();
        p.record.status = OperationStatus::Discarded;
        p.record.error = Some(code);
        p.record.updated_at = now;
        let _ = self.store.upsert_operation(&p.record);
        let ev = Ev {
            vm_id: Some(vm_id),
            run_id: p.record.run_id,
            operation_id: Some(p.record.id),
            session_id: p.session_id(),
            error: Some(code),
            ..Ev::default()
        };
        let actor = p.caller.actor();
        self.emit_after(
            actor,
            AuditAction::OperationDiscard,
            AuditOutcome::Discarded,
            ev,
        );
        let id = p.record.id;
        if let Some(rt) = self.vms.get_mut(&vm_id) {
            rt.remember(p);
        }
        id
    }

    fn set_state(&mut self, vm_id: VmId, to: VmState) -> bool {
        let Some(rt) = self.vms.get_mut(&vm_id) else {
            return false;
        };
        if rt.state == to {
            return true;
        }
        if !rt.state.can_transition_to(to) {
            return false;
        }
        rt.state = to;
        rt.record.persisted_state = to;
        let _ = self.store.set_persisted_state(vm_id, to);
        let run_id = rt.run_id;
        self.emit_after(
            Actor::System,
            AuditAction::VmStateObserved,
            AuditOutcome::Succeeded,
            Ev {
                vm_id: Some(vm_id),
                run_id,
                ..Ev::default()
            },
        );
        true
    }

    /// Closes the current run: state `stopped` (or `failed`), queue discarded.
    fn end_run(&mut self, vm_id: VmId, final_state: VmState, default_reason: RunEndReason) {
        let now = self.clock.now();
        let Some(rt) = self.vms.get_mut(&vm_id) else {
            return;
        };
        let reason = rt.pending_end.take().unwrap_or(default_reason);
        let run = rt.run_id.take();
        rt.geometry = None;
        rt.run_network = None;
        let queued: Vec<Pending> = rt.queue.drain(..).collect();
        if rt.state == VmState::Running && final_state == VmState::Stopped {
            self.set_state(vm_id, VmState::Stopping);
        }
        if rt_state(&self.vms, vm_id) == Some(VmState::Starting) && final_state == VmState::Stopped
        {
            self.set_state(vm_id, VmState::Stopping);
        }
        self.set_state(vm_id, final_state);
        if let Some(run) = run {
            let _ = self.store.finish_run(run, now, reason);
        }
        for p in queued {
            self.discard(vm_id, p, ErrorCode::StateConflict);
        }
    }

    /// Next operation to forward for the VM, after the second check.
    /// `None` if the queue is empty or another operation is already forwarded (a
    /// forced stop can be forwarded even while another operation is in
    /// progress, so as not to stay stuck behind a shutdown that never ends).
    pub fn begin_dispatch(&mut self, vm_id: VmId) -> Result<Option<Dispatch>, PublicError> {
        loop {
            let now = self.clock.now();
            let rt = self
                .vms
                .get_mut(&vm_id)
                .ok_or_else(|| PublicError::not_found("VM does not exist"))?;
            let front_is_force = rt
                .queue
                .front()
                .is_some_and(|p| p.record.kind == OperationKind::ForceStop);
            // User input while a graceful stop is forwarded (the guest's
            // power-off confirmation): forwarded alongside the stop.
            let front_is_input_while_stopping = rt.state == VmState::Stopping
                && rt
                    .queue
                    .front()
                    .is_some_and(|p| p.record.kind == OperationKind::Input);
            if !rt.in_flight.is_empty() && !front_is_force && !front_is_input_while_stopping {
                return Ok(None);
            }
            let Some(p) = rt.queue.pop_front() else {
                return Ok(None);
            };
            let kind = p.record.kind;
            let state = rt.state;
            let current_run = rt.run_id;
            let current_end = rt.pending_end;

            // Second check, immediately before forwarding.
            let effective = match self.authorize(&p.caller, Some(vm_id), p.action.capability(), now)
            {
                Ok(e) => e,
                Err(err) => {
                    self.discard(vm_id, p, err.code);
                    continue;
                }
            };
            if kind == OperationKind::Input
                && let Err(err) = self.check_input_owner(&p.caller, vm_id)
            {
                self.discard(vm_id, p, err.code);
                continue;
            }
            if p.record.run_id.is_some() && p.record.run_id != current_run {
                self.discard(vm_id, p, ErrorCode::StateConflict);
                continue;
            }
            let allowed = if kind == OperationKind::Input {
                input_allowed_for(&p.caller, state)
            } else {
                action_allowed_in(kind, state)
            };
            if !allowed {
                self.discard(vm_id, p, ErrorCode::StateConflict);
                continue;
            }
            let mut start = None;
            let run_id = if kind == OperationKind::Start {
                if let Err(err) = self.check_start_resources(vm_id) {
                    self.discard(vm_id, p, err.code);
                    continue;
                }
                let mut vm = self
                    .vms
                    .get(&vm_id)
                    .map(|rt| rt.record.clone())
                    .ok_or_else(|| PublicError::not_found("VM does not exist"))?;
                // ADR-0033: the network of this run. The backend builds the
                // NIC from the mode of the plan, not from the settings.
                vm.config.network = self.start_network(vm_id, &vm.config);
                let mut media = Vec::new();
                let mut missing = false;
                for id in [vm.config.install_media, vm.config.data_media]
                    .into_iter()
                    .flatten()
                {
                    match self.store.get_media(id) {
                        Ok(Some(e)) => media.push(e),
                        Ok(None) => missing = true,
                        Err(err) => {
                            // The operation is not lost: discarded with the error.
                            self.discard(vm_id, p, err.code);
                            return Err(err);
                        }
                    }
                }
                if missing {
                    self.discard(vm_id, p, ErrorCode::NotFound);
                    continue;
                }
                let run = self.ids.run_id();
                if let Err(err) = self.store.insert_run(&RunRecord {
                    id: run,
                    vm_id,
                    config_revision: vm.config_revision,
                    baseline_id: None,
                    started_at: now,
                    ended_at: None,
                    end_reason: None,
                }) {
                    self.discard(vm_id, p, err.code);
                    return Err(err);
                }
                if let Some(rt) = self.vms.get_mut(&vm_id) {
                    rt.run_id = Some(run);
                    rt.geometry = None;
                    rt.run_network = Some(vm.config.network);
                }
                self.set_state(vm_id, VmState::Starting);
                start = Some(StartPlan { vm, media });
                run
            } else {
                let Some(run) = current_run else {
                    self.discard(vm_id, p, ErrorCode::StateConflict);
                    continue;
                };
                if matches!(kind, OperationKind::Stop | OperationKind::ForceStop)
                    && state != VmState::Stopping
                {
                    if let Some(rt) = self.vms.get_mut(&vm_id) {
                        rt.pending_end = Some(if kind == OperationKind::Stop {
                            RunEndReason::Stopped
                        } else {
                            RunEndReason::ForceStopped
                        });
                    }
                    self.set_state(vm_id, VmState::Stopping);
                } else if kind == OperationKind::ForceStop
                    && let Some(rt) = self.vms.get_mut(&vm_id)
                {
                    rt.pending_end = Some(RunEndReason::ForceStopped);
                }
                run
            };

            let mut p = p;
            p.record.status = OperationStatus::Forwarded;
            p.record.run_id = Some(run_id);
            p.record.updated_at = now;
            if let Err(err) = self.store.upsert_operation(&p.record) {
                // No forwarding: the VM goes back to the previous state instead of
                // staying in `starting`/`stopping` without operations, and
                // the operation is discarded with the error.
                if let Some(rt) = self.vms.get_mut(&vm_id) {
                    rt.run_id = current_run;
                    rt.pending_end = current_end;
                    rt.state = state;
                    rt.record.persisted_state = state;
                }
                let _ = self.store.set_persisted_state(vm_id, state);
                self.discard(vm_id, p, err.code);
                return Err(err);
            }
            let ev = Ev {
                vm_id: Some(vm_id),
                run_id: Some(run_id),
                operation_id: Some(p.record.id),
                session_id: p.session_id(),
                ..Ev::default()
            };
            self.emit_after(
                p.caller.actor(),
                audit_action_for(kind),
                AuditOutcome::Forwarded,
                ev,
            );
            let dispatch = Dispatch {
                operation_id: p.record.id,
                vm_id,
                run_id,
                action: p.action.clone(),
                start,
                max_screenshot_bytes: effective.quotas.max_screenshot_bytes,
            };
            if let Some(rt) = self.vms.get_mut(&vm_id) {
                rt.in_flight.push(p);
            }
            return Ok(Some(dispatch));
        }
    }

    /// Records the outcome of a forwarded operation.
    pub fn finish_dispatch(
        &mut self,
        vm_id: VmId,
        operation_id: OperationId,
        result: Result<DispatchOutcome, PublicError>,
    ) -> Result<OperationRecord, PublicError> {
        let now = self.clock.now();
        let rt = self
            .vms
            .get_mut(&vm_id)
            .ok_or_else(|| PublicError::not_found("VM does not exist"))?;
        let idx = rt
            .in_flight
            .iter()
            .position(|p| p.record.id == operation_id)
            .ok_or_else(|| PublicError::not_found("operation not forwarded"))?;
        let mut p = rt.in_flight.remove(idx);
        let kind = p.record.kind;
        let op_run = p.record.run_id;
        let is_current = op_run.is_some() && op_run == rt.run_id;

        let result = match (result, kind) {
            (Ok(DispatchOutcome::Screenshot(shot)), OperationKind::Screenshot) => {
                let valid = shot.vm_id == vm_id
                    && Some(shot.run_id) == op_run
                    && shot.geometry.validate().is_ok();
                if valid {
                    if is_current {
                        rt.geometry = Some(shot.geometry);
                    }
                    Ok(())
                } else {
                    Err(PublicError::infrastructure(
                        "screenshot inconsistent with the request",
                    ))
                }
            }
            (Ok(DispatchOutcome::Screenshot(_)), _) => Err(PublicError::infrastructure(
                "backend outcome inconsistent with the operation",
            )),
            (Ok(DispatchOutcome::Done), OperationKind::Screenshot) => Err(
                PublicError::infrastructure("screenshot missing from the backend outcome"),
            ),
            (Ok(DispatchOutcome::Done), _) => Ok(()),
            (Err(e), _) => Err(e),
        };

        p.record.status = if result.is_ok() {
            OperationStatus::Succeeded
        } else {
            OperationStatus::Failed
        };
        p.record.error = result.as_ref().err().map(|e| e.code);
        p.record.updated_at = now;
        // A catalog error here does not undo what the backend has already
        // done: the in-memory state follows the outcome and the error is returned
        // afterwards.
        let persisted = self.store.upsert_operation(&p.record);
        let record = p.record.clone();
        let actor = p.caller.actor();
        let session_id = p.session_id();
        if let Some(rt) = self.vms.get_mut(&vm_id) {
            rt.remember(p);
        }

        if is_current {
            match (kind, &result) {
                (OperationKind::Start, Ok(())) => {
                    self.set_state(vm_id, VmState::Running);
                }
                (OperationKind::Start, Err(_)) => {
                    self.end_run(vm_id, VmState::Failed, RunEndReason::StartFailed);
                }
                (OperationKind::Stop | OperationKind::ForceStop, Ok(())) => {
                    self.end_run(vm_id, VmState::Stopped, RunEndReason::Stopped);
                }
                (OperationKind::ForceStop, Err(_)) => {
                    // Process end not confirmed: reconciliation decides.
                    self.set_state(vm_id, VmState::Failed);
                }
                // Graceful stop whose escalation did not observe the exit in
                // time: the run stays current and the VM stays `stopping`
                // (console and user input still allowed, a forced stop or an
                // Emergency Stop can still be requested). The backend keeps
                // the run "awaiting exit" and reports the exit whatever its
                // cause; the observation (`process_exited`) then closes the
                // run as `stopped` with the pending reason (`stopped`).
                (OperationKind::Stop, Err(_)) => {}
                _ => {}
            }
        }
        let ev = Ev {
            vm_id: Some(vm_id),
            run_id: op_run,
            operation_id: Some(operation_id),
            session_id,
            error: record.error,
            ..Ev::default()
        };
        let outcome = if result.is_ok() {
            AuditOutcome::Succeeded
        } else {
            AuditOutcome::Failed
        };
        self.emit_after(actor, audit_action_for(kind), outcome, ev);
        persisted?;
        Ok(record)
    }

    /// Synchronous helper: forwards the next operation to the backend and records
    /// its outcome. For `start` it inspects and verifies the media before use.
    pub fn execute_next<B: VmBackend, M: MediaInspector>(
        &mut self,
        vm_id: VmId,
        backend: &mut B,
        inspector: &mut M,
    ) -> Result<Option<OperationRecord>, PublicError> {
        let Some(d) = self.begin_dispatch(vm_id)? else {
            return Ok(None);
        };
        self.execute_dispatch(d, backend, inspector).map(Some)
    }

    /// Synchronous helper: forwards an operation obtained from
    /// [`Self::begin_dispatch`] to the backend and records its outcome with
    /// [`Self::finish_dispatch`]. For `start` it inspects and verifies the
    /// media before use.
    pub fn execute_dispatch<B: VmBackend, M: MediaInspector>(
        &mut self,
        d: Dispatch,
        backend: &mut B,
        inspector: &mut M,
    ) -> Result<OperationRecord, PublicError> {
        let vm_id = d.vm_id;
        let result = match &d.action {
            VmAction::Start => {
                let now = self.clock.now();
                let plan = d.start.as_ref();
                let verified: Result<Vec<_>, PublicError> = plan
                    .map(|p| p.media.as_slice())
                    .unwrap_or_default()
                    .iter()
                    .map(|e| inspector.inspect(e).and_then(|o| verify_media(e, &o, now)))
                    .collect();
                match (verified, plan) {
                    (Ok(v), Some(plan)) => backend
                        .start(&plan.vm, d.run_id, &v)
                        .map(|()| DispatchOutcome::Done),
                    (Err(e), _) => Err(e),
                    (Ok(_), None) => Err(PublicError::infrastructure("start plan missing")),
                }
            }
            VmAction::Stop => backend
                .shutdown(vm_id, d.run_id)
                .map(|()| DispatchOutcome::Done),
            VmAction::ForceStop => backend
                .force_stop(vm_id, d.run_id)
                .map(|()| DispatchOutcome::Done),
            VmAction::Reset => backend
                .reset(vm_id, d.run_id)
                .map(|()| DispatchOutcome::Done),
            VmAction::Screenshot => backend
                .screenshot(vm_id, d.run_id, d.max_screenshot_bytes)
                .and_then(|s| {
                    if s.byte_len > d.max_screenshot_bytes {
                        Err(PublicError::resource_limit(
                            "screenshot exceeds the byte limit",
                        ))
                    } else {
                        Ok(DispatchOutcome::Screenshot(s))
                    }
                }),
            VmAction::Input(batch) => backend
                .send_input(vm_id, d.run_id, batch.geometry, &batch.events)
                .map(|()| DispatchOutcome::Done),
        };
        self.finish_dispatch(vm_id, d.operation_id, result)
    }

    /// Start refused **before** the backend was called (F3: the disks of
    /// the current state failed verification, a writer holds them open...):
    /// the operation fails with `err`, the run is closed and the VM goes back
    /// to `stopped` instead of `failed`, since no process was started (the
    /// user can restore another baseline or delete the bad one).
    pub fn refuse_start(
        &mut self,
        vm_id: VmId,
        operation_id: OperationId,
        err: PublicError,
    ) -> Result<OperationRecord, PublicError> {
        let now = self.clock.now();
        let rt = self
            .vms
            .get_mut(&vm_id)
            .ok_or_else(|| PublicError::not_found("VM does not exist"))?;
        let idx = rt
            .in_flight
            .iter()
            .position(|p| p.record.id == operation_id && p.record.kind == OperationKind::Start)
            .ok_or_else(|| PublicError::not_found("start not forwarded"))?;
        let mut p = rt.in_flight.remove(idx);
        let is_current = p.record.run_id.is_some() && p.record.run_id == rt.run_id;
        p.record.status = OperationStatus::Failed;
        p.record.error = Some(err.code);
        p.record.updated_at = now;
        let persisted = self.store.upsert_operation(&p.record);
        let record = p.record.clone();
        let actor = p.caller.actor();
        let session_id = p.session_id();
        if let Some(rt) = self.vms.get_mut(&vm_id) {
            rt.remember(p);
        }
        if is_current {
            self.end_run(vm_id, VmState::Stopped, RunEndReason::StartFailed);
        }
        self.emit_after(
            actor,
            AuditAction::VmStart,
            AuditOutcome::Failed,
            Ev {
                vm_id: Some(vm_id),
                run_id: record.run_id,
                operation_id: Some(operation_id),
                session_id,
                error: Some(err.code),
                ..Ev::default()
            },
        );
        persisted?;
        Ok(record)
    }

    // ----- baselines (F3) -------------------------------------------------

    /// Checks shared by the baseline requests: user only (agent tools are
    /// F4/F5), VM `stopped`, nothing queued or forwarded, no storage
    /// operation in progress. Refusals are audited as `denied`.
    fn check_baseline_request(
        &mut self,
        caller: &Caller,
        vm_id: VmId,
        action: AuditAction,
    ) -> Result<VmRecord, PublicError> {
        let actor = caller.actor();
        let ev = || Ev {
            vm_id: Some(vm_id),
            ..Ev::default()
        };
        if let Caller::Agent { session_id, .. } = caller {
            // F5: an agent session may create a checkpoint (`vm.snapshot`)
            // or restore (`vm.restore`) on the VM of its grant, with the
            // explicit capability; deleting baselines stays with the user.
            let capability = match action {
                AuditAction::BaselineCreate => Some(Capability::VmSnapshot),
                AuditAction::BaselineRestore => Some(Capability::VmRestore),
                _ => None,
            };
            let Some(capability) = capability else {
                let err =
                    PublicError::permission_denied("baselines not available to agent sessions");
                return Err(self.deny(actor, action, ev(), err));
            };
            let now = self.clock.now();
            if let Err(err) = self.authorize(caller, Some(vm_id), capability, now) {
                let ev = Ev {
                    session_id: Some(*session_id),
                    capability: Some(capability),
                    ..ev()
                };
                return Err(self.deny(actor, action, ev, err));
            }
            if self
                .sessions
                .get(*session_id)
                .is_some_and(|s| s.state == SessionState::Paused)
            {
                let err = PublicError::permission_denied("session paused");
                return Err(self.deny(actor, action, ev(), err));
            }
        }
        let check = match self.vms.get(&vm_id) {
            None => Err(PublicError::not_found("VM does not exist")),
            Some(rt) if rt.storage_op.is_some() => Err(PublicError::vm_busy(
                "storage operation in progress on the VM",
            )),
            Some(rt) if rt.state != VmState::Stopped => Err(PublicError::state_conflict(
                "baselines require a stopped VM",
            )
            .with_detail("state", rt.state.as_str())),
            Some(rt) if !rt.queue.is_empty() || !rt.in_flight.is_empty() => {
                Err(PublicError::vm_busy("operations in progress on the VM"))
            }
            Some(rt) => Ok(rt.record.clone()),
        };
        check.map_err(|err| self.deny(actor, action, ev(), err))
    }

    /// First step of a long storage operation on a stopped VM (F3):
    /// `kind` is [`OperationKind::Snapshot`] (baseline creation) or
    /// [`OperationKind::Restore`]. After the checks of
    /// [`Self::check_baseline_request`], `begin` runs on the catalog (journal
    /// intents, guards); on success the VM stays busy (`vm_busy` for every
    /// other mutation, [`VmStatus::storage_operation`] set) until
    /// [`Self::finish_storage_operation`]. Audited: `accepted`, or `failed`
    /// if `begin` fails.
    pub fn begin_storage_operation<T>(
        &mut self,
        caller: &Caller,
        vm_id: VmId,
        kind: OperationKind,
        begin: impl FnOnce(&mut S, &VmRecord, OperationId) -> Result<T, PublicError>,
    ) -> Result<(OperationId, T), PublicError> {
        if !matches!(kind, OperationKind::Snapshot | OperationKind::Restore) {
            return Err(PublicError::invalid_argument(
                "not a storage operation kind",
            ));
        }
        let action = audit_action_for(kind);
        let actor = caller.actor();
        let vm = self.check_baseline_request(caller, vm_id, action)?;
        let op = self.ids.operation_id();
        let ev = |error| Ev {
            vm_id: Some(vm_id),
            operation_id: Some(op),
            error,
            ..Ev::default()
        };
        let out = match begin(&mut self.store, &vm, op) {
            Ok(t) => t,
            Err(err) => {
                self.emit_after(actor, action, AuditOutcome::Failed, ev(Some(err.code)));
                return Err(err);
            }
        };
        let now = self.clock.now();
        let record = OperationRecord {
            id: op,
            vm_id,
            run_id: None,
            session_id: None,
            kind,
            status: OperationStatus::Forwarded,
            error: None,
            requested_at: now,
            updated_at: now,
        };
        let _ = self.store.upsert_operation(&record);
        self.op_index.insert(op, vm_id);
        if let Some(rt) = self.vms.get_mut(&vm_id) {
            rt.storage_op = Some(StorageOp {
                record,
                caller: caller.clone(),
            });
        }
        self.emit_after(actor, action, AuditOutcome::Accepted, ev(None));
        Ok((op, out))
    }

    /// Last step of a storage operation started with
    /// [`Self::begin_storage_operation`]: `finish` publishes (or discards)
    /// the result on the catalog; the operation is recorded and audited
    /// (`succeeded`/`failed`) and the VM is free again. A successful restore
    /// closes the control sessions of the VM (ROADMAP 8: the old run's
    /// requests are invalid; cause `restored`).
    pub fn finish_storage_operation<T>(
        &mut self,
        vm_id: VmId,
        operation_id: OperationId,
        finish: impl FnOnce(&mut S) -> Result<T, PublicError>,
    ) -> Result<T, PublicError> {
        let op = self
            .vms
            .get_mut(&vm_id)
            .and_then(|rt| {
                if rt.storage_op.as_ref().map(|o| o.record.id) == Some(operation_id) {
                    rt.storage_op.take()
                } else {
                    None
                }
            })
            .ok_or_else(|| PublicError::not_found("storage operation not in progress"))?;
        let result = finish(&mut self.store);
        let now = self.clock.now();
        let StorageOp { mut record, caller } = op;
        record.status = if result.is_ok() {
            OperationStatus::Succeeded
        } else {
            OperationStatus::Failed
        };
        record.error = result.as_ref().err().map(|e| e.code);
        record.updated_at = now;
        let _ = self.store.upsert_operation(&record);
        // Not kept in the idempotency history (not a queue request); the
        // identifier stays reserved in the operation index.
        let kind = record.kind;
        let error = record.error;
        if kind == OperationKind::Restore && result.is_ok() {
            // The control sessions of the VM close (ROADMAP 8), except the
            // agent session that asked for the restore itself (F5: the
            // `vm.restore` tool works within the session's limits).
            let requester = match &caller {
                Caller::Agent { session_id, .. } => Some(*session_id),
                Caller::User => None,
            };
            for s in self.sessions.live_for_vm(vm_id) {
                if Some(s) != requester {
                    self.revoke_inner(s, TerminationCause::Restored);
                }
            }
            if let Some(rt) = self.vms.get_mut(&vm_id) {
                rt.geometry = None;
            }
        }
        let outcome = if result.is_ok() {
            AuditOutcome::Succeeded
        } else {
            AuditOutcome::Failed
        };
        self.emit_after(
            caller.actor(),
            audit_action_for(kind),
            outcome,
            Ev {
                vm_id: Some(vm_id),
                operation_id: Some(operation_id),
                error,
                ..Ev::default()
            },
        );
        result
    }

    /// Deletion of a baseline (F3): user only, VM `stopped` and free; `f`
    /// deletes rows and files through the catalog journal (refusing a
    /// baseline in use or protected). Audited (`baseline_delete`).
    pub fn delete_baseline_with(
        &mut self,
        caller: &Caller,
        vm_id: VmId,
        f: impl FnOnce(&mut S) -> Result<(), PublicError>,
    ) -> Result<(), PublicError> {
        let action = AuditAction::BaselineDelete;
        self.check_baseline_request(caller, vm_id, action)?;
        let r = f(&mut self.store);
        let (outcome, error) = match &r {
            Ok(()) => (AuditOutcome::Succeeded, None),
            Err(e) => (AuditOutcome::Failed, Some(e.code)),
        };
        self.emit_after(
            caller.actor(),
            action,
            outcome,
            Ev {
                vm_id: Some(vm_id),
                error,
                ..Ev::default()
            },
        );
        r
    }

    // ----- user snapshots (ADR-0032) -------------------------------------

    /// Why user snapshots of the VM cannot be changed now, for the UI:
    /// `None` when they can (the storage adds its own reasons, such as a VM
    /// running on an overlay of a baseline).
    #[must_use]
    pub fn snapshot_unavailable_reason(&self, vm_id: VmId) -> Option<&'static str> {
        if !self.sessions.live_for_vm(vm_id).is_empty() {
            return Some(SNAPSHOT_AGENT_SESSION);
        }
        None
    }

    /// Checks shared by the user snapshot requests: the user only (agent
    /// sessions never touch snapshots: `permission_denied`), no live agent
    /// session on the VM (`state_conflict`, reason `agent_session`), nothing
    /// queued or forwarded and no storage operation (`vm_busy`), the VM
    /// `stopped`, or `running` when `allow_running`. Refusals are audited
    /// as `denied`. Returns the record and the current run.
    fn check_snapshot_request(
        &mut self,
        caller: &Caller,
        vm_id: VmId,
        action: AuditAction,
        allow_running: bool,
    ) -> Result<(VmRecord, Option<RunId>), PublicError> {
        let actor = caller.actor();
        let ev = Ev {
            vm_id: Some(vm_id),
            session_id: agent_session(caller),
            ..Ev::default()
        };
        if matches!(caller, Caller::Agent { .. }) {
            let err =
                PublicError::permission_denied("snapshots are not available to agent sessions");
            return Err(self.deny(actor, action, ev, err));
        }
        let live = !self.sessions.live_for_vm(vm_id).is_empty();
        let check = match self.vms.get(&vm_id) {
            None => Err(PublicError::not_found("VM does not exist")),
            Some(_) if live => Err(PublicError::state_conflict(SNAPSHOT_AGENT_SESSION)
                .with_detail("reason", "agent_session")),
            Some(rt) if rt.storage_op.is_some() => Err(PublicError::vm_busy(
                "storage operation in progress on the VM",
            )),
            Some(rt) if !rt.queue.is_empty() || !rt.in_flight.is_empty() => {
                Err(PublicError::vm_busy("operations in progress on the VM"))
            }
            Some(rt) if rt.state == VmState::Stopped => Ok((rt.record.clone(), None)),
            Some(rt) if rt.state == VmState::Running && allow_running => {
                Ok((rt.record.clone(), rt.run_id))
            }
            Some(rt) => Err(PublicError::state_conflict(if allow_running {
                "snapshots need the VM stopped or running"
            } else {
                "reverting to a snapshot needs the VM stopped: shut it down first"
            })
            .with_detail("state", rt.state.as_str())),
        };
        check.map_err(|err| self.deny(actor, action, ev, err))
    }

    /// First step of a user snapshot operation (ADR-0032): `kind` is
    /// [`OperationKind::UserSnapshot`] or [`OperationKind::UserSnapshotDelete`]
    /// (VM `stopped` or `running`) or [`OperationKind::UserSnapshotRevert`]
    /// (VM `stopped`). After the checks of
    /// [`Self::check_snapshot_request`], `begin` runs on the catalog with the
    /// VM record and its current run (journal intent); on success the VM is
    /// busy until [`Self::finish_snapshot_operation`]. Audited: `accepted`,
    /// or `failed` if `begin` fails.
    pub fn begin_snapshot_operation<T>(
        &mut self,
        caller: &Caller,
        vm_id: VmId,
        kind: OperationKind,
        begin: impl FnOnce(&mut S, &VmRecord, Option<RunId>) -> Result<T, PublicError>,
    ) -> Result<(OperationId, T), PublicError> {
        let allow_running = match kind {
            OperationKind::UserSnapshot | OperationKind::UserSnapshotDelete => true,
            OperationKind::UserSnapshotRevert => false,
            _ => {
                return Err(PublicError::invalid_argument(
                    "not a snapshot operation kind",
                ));
            }
        };
        let action = audit_action_for(kind);
        let actor = caller.actor();
        let (vm, run) = self.check_snapshot_request(caller, vm_id, action, allow_running)?;
        let op = self.ids.operation_id();
        let ev = |error| Ev {
            vm_id: Some(vm_id),
            run_id: run,
            operation_id: Some(op),
            error,
            ..Ev::default()
        };
        let out = match begin(&mut self.store, &vm, run) {
            Ok(t) => t,
            Err(err) => {
                self.emit_after(actor, action, AuditOutcome::Failed, ev(Some(err.code)));
                return Err(err);
            }
        };
        let now = self.clock.now();
        let record = OperationRecord {
            id: op,
            vm_id,
            run_id: run,
            session_id: None,
            kind,
            status: OperationStatus::Forwarded,
            error: None,
            requested_at: now,
            updated_at: now,
        };
        let _ = self.store.upsert_operation(&record);
        self.op_index.insert(op, vm_id);
        if let Some(rt) = self.vms.get_mut(&vm_id) {
            rt.storage_op = Some(StorageOp {
                record,
                caller: caller.clone(),
            });
        }
        self.emit_after(actor, action, AuditOutcome::Accepted, ev(None));
        Ok((op, out))
    }

    /// Last step of a user snapshot operation: `finish` settles it on the
    /// catalog and may return the VM record written by a revert (the
    /// snapshot's configuration, new revision), which becomes the core's.
    /// Recorded and audited (`succeeded`/`failed`); the VM is free again.
    pub fn finish_snapshot_operation<T>(
        &mut self,
        vm_id: VmId,
        operation_id: OperationId,
        finish: impl FnOnce(&mut S) -> Result<(T, Option<VmRecord>), PublicError>,
    ) -> Result<T, PublicError> {
        let mut new_record = None;
        let r = self.finish_storage_operation(vm_id, operation_id, |store| {
            let (t, rec) = finish(store)?;
            new_record = rec;
            Ok(t)
        });
        if let Some(rec) = new_record
            && let Some(rt) = self.vms.get_mut(&vm_id)
        {
            rt.record = rec;
            rt.geometry = None;
        }
        r
    }

    /// The configuration a revert to a snapshot gives the VM: the
    /// snapshot's `config` with the VM's current name, at revision + 1.
    /// Media no longer in the registry are detached; a configuration beyond
    /// the current limits or for a guest this host no longer runs is
    /// refused (`invalid_argument`/`unsupported`).
    pub fn reverted_record(
        &self,
        vm_id: VmId,
        mut config: VmConfig,
    ) -> Result<VmRecord, PublicError> {
        let rt = self
            .vms
            .get(&vm_id)
            .ok_or_else(|| PublicError::not_found("VM does not exist"))?;
        config.name = rt.record.config.name.clone();
        for slot in [&mut config.install_media, &mut config.data_media] {
            if let Some(id) = *slot
                && self.store.get_media(id)?.is_none()
            {
                *slot = None;
            }
        }
        self.check_config_host(&config)
            .and_then(|()| validate_config(&config, &self.effective_limits()))
            .and_then(|()| self.check_config_media(&config))
            .map_err(|e| {
                PublicError::state_conflict(format!(
                    "the snapshot's configuration is not valid on this host any more: {}",
                    e.message
                ))
                .with_detail("reason", "snapshot_config")
            })?;
        Ok(VmRecord {
            config,
            config_revision: rt.record.config_revision.saturating_add(1),
            ..rt.record.clone()
        })
    }

    /// Reads or renames user snapshots of a VM: the user only (audited
    /// `snapshot_update` when `update`; a read by an agent is denied).
    pub fn snapshot_access<T>(
        &mut self,
        caller: &Caller,
        vm_id: VmId,
        update: bool,
        f: impl FnOnce(&mut S) -> Result<T, PublicError>,
    ) -> Result<T, PublicError> {
        let action = AuditAction::SnapshotUpdate;
        let actor = caller.actor();
        let ev = Ev {
            vm_id: Some(vm_id),
            session_id: agent_session(caller),
            ..Ev::default()
        };
        if matches!(caller, Caller::Agent { .. }) {
            let err =
                PublicError::permission_denied("snapshots are not available to agent sessions");
            return Err(self.deny(actor, action, ev, err));
        }
        if !self.vms.contains_key(&vm_id) {
            return Err(PublicError::not_found("VM does not exist"));
        }
        let r = f(&mut self.store);
        if update {
            let (outcome, error) = match &r {
                Ok(_) => (AuditOutcome::Succeeded, None),
                Err(e) => (AuditOutcome::Failed, Some(e.code)),
            };
            self.emit_after(actor, action, outcome, Ev { error, ..ev });
        }
        r
    }

    /// Read access to the catalog for a caller allowed to see the VM (the
    /// user; agents need `vm.status` on the VM).
    pub fn with_visible_vm<T>(
        &mut self,
        caller: &Caller,
        vm_id: VmId,
        f: impl FnOnce(&S) -> Result<T, PublicError>,
    ) -> Result<T, PublicError> {
        self.vm_status(caller, vm_id)?;
        f(&self.store)
    }

    // ----- observations and reconciliation --------------------------------

    /// Applies a backend observation. Observations referring to
    /// a run that is not current are ignored (`Ok(false)`).
    pub fn observe(&mut self, vm_id: VmId, obs: BackendObservation) -> Result<bool, PublicError> {
        let rt = self
            .vms
            .get(&vm_id)
            .ok_or_else(|| PublicError::not_found("VM does not exist"))?;
        if rt.run_id != Some(obs.run_id()) {
            return Ok(false);
        }
        let state = rt.state;
        match obs {
            BackendObservation::ProcessSpawned { .. } => {}
            BackendObservation::QmpReady { .. } => {
                if state == VmState::Starting {
                    self.set_state(vm_id, VmState::Running);
                }
            }
            BackendObservation::StartFailed { .. } => {
                self.end_run(vm_id, VmState::Failed, RunEndReason::StartFailed);
            }
            BackendObservation::ProcessExited { expected, .. } => {
                let pending_end = self.vms.get(&vm_id).and_then(|rt| rt.pending_end);
                if expected || state == VmState::Stopping || pending_end.is_some() {
                    self.end_run(vm_id, VmState::Stopped, RunEndReason::Stopped);
                } else {
                    self.end_run(vm_id, VmState::Failed, RunEndReason::UnexpectedExit);
                    // F6: the run the sessions were controlling is gone
                    // without anyone asking for it (QEMU crashed or was
                    // killed from outside): the sessions of the VM end with
                    // `vm_terminated`, whose report outcome is
                    // `infrastructure_error`. A stop asked by the user or an
                    // agent, or the Emergency Stop, keeps its own cause.
                    for s in self.sessions.live_for_vm(vm_id) {
                        self.revoke_inner(s, TerminationCause::VmTerminated);
                    }
                }
            }
        }
        Ok(true)
    }

    /// Records the current display geometry (for example from the RFB client).
    pub fn observe_geometry(
        &mut self,
        vm_id: VmId,
        run_id: RunId,
        geometry: DisplayGeometry,
    ) -> Result<bool, PublicError> {
        geometry.validate()?;
        let rt = self
            .vms
            .get_mut(&vm_id)
            .ok_or_else(|| PublicError::not_found("VM does not exist"))?;
        if rt.run_id != Some(run_id) {
            return Ok(false);
        }
        rt.geometry = Some(geometry);
        Ok(true)
    }

    /// `failed → recovering`: start of reconciliation.
    pub fn begin_recovery(&mut self, vm_id: VmId) -> Result<(), PublicError> {
        let state = rt_state(&self.vms, vm_id)
            .ok_or_else(|| PublicError::not_found("VM does not exist"))?;
        if state == VmState::Recovering {
            return Ok(());
        }
        if !self.set_state(vm_id, VmState::Recovering) {
            return Err(PublicError::state_conflict(
                "reconciliation not allowed in the current state",
            )
            .with_detail("state", state.as_str()));
        }
        Ok(())
    }

    /// Reconciliation at the start of a new supervisor, when the caller
    /// guarantees that no process of a previous run is alive
    /// (Windows: the Job Object of the previous supervisor is closed and its
    /// processes terminated). VMs in `recovering` with no open intents in the
    /// journal go back to `stopped`; the others stay in reconciliation.
    /// Returns the VMs brought back to `stopped`.
    pub fn reconcile_after_restart(&mut self) -> Result<Vec<VmId>, PublicError> {
        let candidates: Vec<VmId> = self
            .vms
            .iter()
            .filter(|(_, rt)| rt.state == VmState::Recovering)
            .map(|(id, _)| *id)
            .collect();
        let mut done = Vec::new();
        for id in candidates {
            if self.store.has_open_intents(id)? {
                continue;
            }
            self.store.close_open_runs(id, RunEndReason::Reconciled)?;
            self.finish_recovery(id)?;
            done.push(id);
        }
        Ok(done)
    }

    /// `recovering → stopped`: reconciliation completed by the supervisor
    /// (absence of processes verified, journal applied).
    pub fn finish_recovery(&mut self, vm_id: VmId) -> Result<(), PublicError> {
        let state = rt_state(&self.vms, vm_id)
            .ok_or_else(|| PublicError::not_found("VM does not exist"))?;
        if state != VmState::Recovering {
            return Err(PublicError::state_conflict("VM not in reconciliation"));
        }
        if let Some(rt) = self.vms.get_mut(&vm_id)
            && rt.run_id.is_some()
        {
            rt.pending_end = Some(RunEndReason::Reconciled);
        }
        self.end_run(vm_id, VmState::Stopped, RunEndReason::Reconciled);
        Ok(())
    }

    // ----- sessions --------------------------------------------------------

    /// Creates an agent session approved by the user and issues its grant and lease.
    pub fn create_session(
        &mut self,
        spec: SessionSpec,
        token: SessionToken,
    ) -> Result<(Grant, Lease), PublicError> {
        let now = self.clock.now();
        let limits = self.effective_limits();
        let live = u32::try_from(self.sessions.live().count()).unwrap_or(u32::MAX);
        let result = (|| {
            if live >= limits.max_agent_sessions {
                return Err(PublicError::resource_limit(
                    "maximum number of agent sessions reached",
                ));
            }
            if spec.expires_at <= now {
                return Err(PublicError::invalid_argument("grant expiry in the past"));
            }
            if spec.vm_allowlist.is_empty() {
                return Err(PublicError::invalid_argument("empty VM allowlist"));
            }
            for vm in &spec.vm_allowlist {
                if !self.vms.contains_key(vm) {
                    return Err(PublicError::not_found("allowlist VM does not exist"));
                }
                self.check_session_network(*vm, &spec)?;
                self.vms[vm]
                    .record
                    .config
                    .tools
                    .validate_grant(&spec.capabilities, &spec.tools)?;
            }
            if !spec.capabilities.contains(&Capability::NetworkEndpoints)
                && !spec.network_endpoints.is_empty()
            {
                return Err(PublicError::invalid_argument(
                    "network endpoints granted without the network.endpoints capability",
                ));
            }
            Ok(())
        })();
        if let Err(err) = result {
            return Err(self.deny(Actor::User, AuditAction::SessionCreate, Ev::default(), err));
        }
        let session_id = self.ids.session_id();
        let lease = Lease::new(session_id, now, spec.lease_millis)?;
        // `network.endpoints` without any endpoint grants nothing: not
        // recorded (ADR-0033).
        let mut spec = spec;
        if spec.network_endpoints.is_empty() {
            spec.capabilities.remove(&Capability::NetworkEndpoints);
        }
        let grant = Grant {
            tools: spec.tools,
            id: self.ids.grant_id(),
            session_id,
            vm_allowlist: spec.vm_allowlist,
            capabilities: spec.capabilities,
            expires_at: spec.expires_at,
            quotas: spec.quotas,
            policy_revision: self.revision,
            network_endpoints: spec.network_endpoints,
        };
        self.sessions
            .insert(session_id, token, grant.clone(), lease, now);
        self.emit_after(
            Actor::User,
            AuditAction::SessionCreate,
            AuditOutcome::Succeeded,
            Ev {
                session_id: Some(session_id),
                ..Ev::default()
            },
        );
        Ok((grant, lease))
    }

    /// ADR-0033 checks of a session on `vm_id`: a VM running with NAT
    /// refuses the session (no hidden change while it runs: stop it first);
    /// granted endpoints must be endpoints of the VM (the capability
    /// without any endpoint is dropped by [`Self::create_session`]).
    fn check_session_network(&self, vm_id: VmId, spec: &SessionSpec) -> Result<(), PublicError> {
        let Some(rt) = self.vms.get(&vm_id) else {
            return Ok(());
        };
        if rt.run_network == Some(NetworkProfile::Nat) {
            return Err(PublicError::state_conflict(
                "the VM is running with NAT networking, which agent sessions cannot use: stop the VM first (its next start uses the endpoint allowlist of the session, or no network)",
            )
            .with_detail("reason", "nat_running"));
        }
        if spec.capabilities.contains(&Capability::NetworkEndpoints) {
            let known: std::collections::BTreeSet<String> = rt
                .record
                .config
                .endpoints
                .iter()
                .map(lightvm_proto::NetworkEndpoint::key)
                .collect();
            if let Some(unknown) = spec.network_endpoints.iter().find(|n| !known.contains(*n)) {
                return Err(PublicError::invalid_argument(format!(
                    "`{unknown}` is not a network endpoint of the VM"
                )));
            }
        }
        Ok(())
    }

    /// Network mode of a run of `vm_id` starting now (ADR-0033): the VM's
    /// mode in manual use; while an agent session targets the VM, the
    /// endpoint allowlist if a live session was granted
    /// `network.endpoints` and the VM has endpoints, otherwise no network
    /// (NAT is never given to a run under an agent session).
    fn start_network(&self, vm_id: VmId, config: &VmConfig) -> NetworkProfile {
        if config.network == NetworkProfile::Offline {
            return NetworkProfile::Offline;
        }
        let sessions = self.sessions.live_for_vm(vm_id);
        if sessions.is_empty() {
            return config.network;
        }
        let granted = sessions.iter().any(|s| {
            self.sessions.get(*s).is_some_and(|s| {
                s.grant.capabilities.contains(&Capability::NetworkEndpoints)
                    && !s.grant.network_endpoints.is_empty()
            })
        });
        if granted && !config.endpoints.is_empty() {
            if config.network == NetworkProfile::FilteredInternet {
                NetworkProfile::FilteredInternet
            } else {
                NetworkProfile::Endpoints
            }
        } else {
            NetworkProfile::Offline
        }
    }

    /// Network mode of the VM's current run (`None` without a run).
    #[must_use]
    pub fn run_network(&self, vm_id: VmId) -> Option<NetworkProfile> {
        self.vms.get(&vm_id).and_then(|rt| rt.run_network)
    }

    /// Endpoint names allowed now on the VM's network (ADR-0033): every
    /// endpoint of the VM in manual use; the union of the endpoints granted
    /// to the live sessions targeting it otherwise. `tainted` = an agent
    /// session targeted the current run before: nothing is allowed until
    /// the next start (no widening after a session ends).
    #[must_use]
    pub fn allowed_endpoints(
        &self,
        vm_id: VmId,
        tainted: bool,
    ) -> std::collections::BTreeSet<String> {
        let Some(rt) = self.vms.get(&vm_id) else {
            return std::collections::BTreeSet::new();
        };
        let sessions = self.sessions.live_for_vm(vm_id);
        if sessions.is_empty() {
            if tainted {
                return std::collections::BTreeSet::new();
            }
            return rt
                .record
                .config
                .endpoints
                .iter()
                .map(lightvm_proto::NetworkEndpoint::key)
                .collect();
        }
        sessions
            .iter()
            .filter_map(|s| self.sessions.get(*s))
            .filter(|s| s.grant.capabilities.contains(&Capability::NetworkEndpoints))
            .flat_map(|s| s.grant.network_endpoints.iter().cloned())
            .collect()
    }

    /// Records a network audit event of a run (ADR-0033): set-up, a
    /// connection that ended, a refused attempt, a change of the allowed
    /// endpoints. The actor is the system; the session is the one targeting
    /// the VM, if any.
    pub fn record_network(
        &mut self,
        vm_id: VmId,
        run_id: Option<RunId>,
        session_id: Option<SessionId>,
        action: AuditAction,
        outcome: AuditOutcome,
        detail: NetworkAudit,
    ) {
        self.emit_after(
            Actor::System,
            action,
            outcome,
            Ev {
                vm_id: Some(vm_id),
                run_id,
                session_id,
                network: Some(detail),
                ..Ev::default()
            },
        );
    }

    /// Records bounded Tools metadata without guest payloads.
    pub fn record_tools(
        &mut self,
        actor: Actor,
        vm: VmId,
        run: RunId,
        action: AuditAction,
        outcome: AuditOutcome,
        detail: lightvm_proto::ToolAudit,
    ) {
        let session_id = match actor {
            Actor::Agent { session_id } => Some(session_id),
            _ => None,
        };
        self.emit_after(
            actor,
            action,
            outcome,
            Ev {
                vm_id: Some(vm),
                run_id: Some(run),
                session_id,
                tools: Some(detail),
                ..Ev::default()
            },
        );
    }

    /// Renews the lease (checked by the supervisor, not by the client).
    pub fn renew_lease(
        &mut self,
        session_id: SessionId,
        token: &SessionToken,
        lease_millis: i64,
    ) -> Result<Lease, PublicError> {
        let now = self.clock.now();
        let r = self.sessions.renew(session_id, token, lease_millis, now);
        let ev = Ev {
            session_id: Some(session_id),
            ..Ev::default()
        };
        let actor = Actor::Agent { session_id };
        match &r {
            Ok(_) => self.emit_after(
                actor,
                AuditAction::SessionLease,
                AuditOutcome::Succeeded,
                ev,
            ),
            Err(e) => {
                let _ = self.deny(actor, AuditAction::SessionLease, ev, e.clone());
            }
        }
        r
    }

    /// Check of a read of screenshot bytes already captured for an agent
    /// session (`vm.screenshot` on the VM, observation: allowed while
    /// paused). Audited (`screenshot_read`), never the bytes.
    pub fn read_screenshot(
        &mut self,
        caller: &Caller,
        vm_id: VmId,
        operation_id: OperationId,
    ) -> Result<(), PublicError> {
        let now = self.clock.now();
        let ev = Ev {
            vm_id: Some(vm_id),
            operation_id: Some(operation_id),
            session_id: agent_session(caller),
            capability: Some(Capability::VmScreenshot),
            ..Ev::default()
        };
        match self.authorize(caller, Some(vm_id), Capability::VmScreenshot, now) {
            Ok(_) => {
                self.emit_after(
                    caller.actor(),
                    AuditAction::ScreenshotRead,
                    AuditOutcome::Succeeded,
                    ev,
                );
                Ok(())
            }
            Err(err) => Err(self.deny(caller.actor(), AuditAction::ScreenshotRead, ev, err)),
        }
    }

    /// Agent pause: blocks forwarding of new mutations (the VM keeps running).
    pub fn pause_session(&mut self, session_id: SessionId) -> Result<(), PublicError> {
        let s = self
            .sessions
            .get_mut(session_id)
            .ok_or_else(|| PublicError::not_found("session does not exist"))?;
        if s.state != SessionState::Active {
            return Err(PublicError::state_conflict("session not active"));
        }
        s.state = SessionState::Paused;
        self.emit_after(
            Actor::User,
            AuditAction::SessionPause,
            AuditOutcome::Succeeded,
            Ev {
                session_id: Some(session_id),
                ..Ev::default()
            },
        );
        Ok(())
    }

    /// Resume after a pause (not after revocation or expiry).
    pub fn resume_session(&mut self, session_id: SessionId) -> Result<(), PublicError> {
        let now = self.clock.now();
        let s = self
            .sessions
            .get_mut(session_id)
            .ok_or_else(|| PublicError::not_found("session does not exist"))?;
        if s.state != SessionState::Paused {
            return Err(PublicError::state_conflict("session not paused"));
        }
        if s.expiry_at(now).is_some() {
            return Err(PublicError::session_expired("session expired"));
        }
        s.state = SessionState::Active;
        self.emit_after(
            Actor::User,
            AuditAction::SessionResume,
            AuditOutcome::Succeeded,
            Ev {
                session_id: Some(session_id),
                ..Ev::default()
            },
        );
        Ok(())
    }

    fn revoke_inner(&mut self, session_id: SessionId, cause: TerminationCause) -> RevocationReport {
        let mut report = RevocationReport::default();
        if !self.sessions.terminate(session_id, cause) {
            return report;
        }
        let vm_ids: Vec<VmId> = self.vms.keys().copied().collect();
        for vm_id in vm_ids {
            let Some(rt) = self.vms.get_mut(&vm_id) else {
                continue;
            };
            let (mine, rest): (VecDeque<Pending>, VecDeque<Pending>) = rt
                .queue
                .drain(..)
                .partition(|p| p.session_id() == Some(session_id));
            rt.queue = rest;
            report.already_forwarded.extend(
                rt.in_flight
                    .iter()
                    .filter(|p| p.session_id() == Some(session_id))
                    .map(|p| p.record.id),
            );
            for p in mine {
                report
                    .discarded
                    .push(self.discard(vm_id, p, discard_code_for(cause)));
            }
        }
        // The input of the session's VMs goes back to the user, keys released.
        let owned: Vec<VmId> = self
            .input_owners
            .iter()
            .filter(|(_, s)| **s == session_id)
            .map(|(v, _)| *v)
            .collect();
        for vm_id in owned {
            self.input_owners.remove(&vm_id);
            self.after_input_owner_change(vm_id, Some(session_id));
            self.emit_after(
                Actor::System,
                AuditAction::InputControl,
                AuditOutcome::Succeeded,
                Ev {
                    vm_id: Some(vm_id),
                    session_id: Some(session_id),
                    capability: Some(Capability::VmInput),
                    termination: Some(cause),
                    ..Ev::default()
                },
            );
        }
        self.emit_after(
            Actor::System,
            AuditAction::SessionTerminate,
            AuditOutcome::Succeeded,
            Ev {
                session_id: Some(session_id),
                termination: Some(cause),
                ..Ev::default()
            },
        );
        report
    }

    /// Revocation: invalidates the session and discards its non-forwarded operations. The
    /// operations already forwarded are listed, not cancelled. No VM stop.
    pub fn revoke_session(
        &mut self,
        session_id: SessionId,
        cause: TerminationCause,
    ) -> Result<SessionTermination, PublicError> {
        if self.sessions.get(session_id).is_none() {
            return Err(PublicError::not_found("session does not exist"));
        }
        let report = self.revoke_inner(session_id, cause);
        Ok(SessionTermination {
            session_id,
            cause,
            report,
            emergency_stops: Vec::new(),
        })
    }

    /// Termination with emergency stop of the session's VMs: used for
    /// expiry, client loss, UI close and resume from sleep (ROADMAP 7.3).
    pub fn terminate_session_with_stop(
        &mut self,
        session_id: SessionId,
        cause: TerminationCause,
    ) -> Result<SessionTermination, PublicError> {
        let (allow, live) = self
            .sessions
            .get(session_id)
            .map(|s| {
                (
                    s.grant.vm_allowlist.clone(),
                    s.state != SessionState::Terminated,
                )
            })
            .ok_or_else(|| PublicError::not_found("session does not exist"))?;
        // An already terminated session (revoked meanwhile, for example) does
        // not stop its VMs a second time.
        if !live {
            return Ok(SessionTermination {
                session_id,
                cause,
                report: RevocationReport::default(),
                emergency_stops: Vec::new(),
            });
        }
        let report = self.revoke_inner(session_id, cause);
        let mut emergency_stops = Vec::new();
        for vm_id in allow {
            if rt_state(&self.vms, vm_id).is_some_and(VmState::has_process) {
                emergency_stops.push(self.emergency_stop(Actor::System, vm_id)?);
            }
        }
        Ok(SessionTermination {
            session_id,
            cause,
            report,
            emergency_stops,
        })
    }

    /// Terminates every live session with `cause`; with `stop` their VMs
    /// are Emergency-Stopped as for [`Self::terminate_session_with_stop`].
    pub fn terminate_all_sessions(
        &mut self,
        cause: TerminationCause,
        stop: bool,
    ) -> Vec<SessionTermination> {
        let live: Vec<SessionId> = self.sessions.live().map(|s| s.id).collect();
        live.into_iter()
            .filter_map(|id| {
                if stop {
                    self.terminate_session_with_stop(id, cause).ok()
                } else {
                    self.revoke_session(id, cause).ok()
                }
            })
            .collect()
    }

    /// Periodic check of grant and lease expiries.
    pub fn tick(&mut self) -> Result<Vec<SessionTermination>, PublicError> {
        let now = self.clock.now();
        let due = self.sessions.due_expirations(now);
        due.into_iter()
            .map(|(id, cause)| self.terminate_session_with_stop(id, cause))
            .collect()
    }

    // ----- Emergency Stop --------------------------------------------------

    /// Emergency Stop: priority path outside the queue. Revokes the sessions that
    /// control the VM, discards the whole queue, lists the requests already forwarded
    /// and moves the VM to `stopping`. The supervisor terminates the QEMU tree with
    /// OS primitives and confirms with `observe(process_exited)`: only then is the VM
    /// `stopped`.
    pub fn emergency_stop(
        &mut self,
        actor: Actor,
        vm_id: VmId,
    ) -> Result<EmergencyStopPlan, PublicError> {
        if !self.vms.contains_key(&vm_id) {
            return Err(PublicError::not_found("VM does not exist"));
        }
        let mut report = RevocationReport::default();
        let terminated = self.sessions.live_for_vm(vm_id);
        for sid in &terminated {
            let r = self.revoke_inner(*sid, TerminationCause::EmergencyStop);
            report.discarded.extend(r.discarded);
            report.already_forwarded.extend(r.already_forwarded);
        }
        let rt = self
            .vms
            .get_mut(&vm_id)
            .ok_or_else(|| PublicError::not_found("VM does not exist"))?;
        let queued: Vec<Pending> = rt.queue.drain(..).collect();
        for p in &rt.in_flight {
            if !report.already_forwarded.contains(&p.record.id) {
                report.already_forwarded.push(p.record.id);
            }
        }
        let run_id = if rt.state.has_process() {
            rt.run_id
        } else {
            None
        };
        if run_id.is_some() {
            rt.pending_end = Some(RunEndReason::EmergencyStop);
        }
        // Remaining operations (from the user or from sessions not bound to the VM): the VM
        // is stopping, hence `state_conflict` as for the end of a run.
        for p in queued {
            report
                .discarded
                .push(self.discard(vm_id, p, ErrorCode::StateConflict));
        }
        if run_id.is_some() {
            self.set_state(vm_id, VmState::Stopping);
        }
        self.emit_after(
            actor,
            AuditAction::EmergencyStop,
            AuditOutcome::Accepted,
            Ev {
                vm_id: Some(vm_id),
                run_id,
                ..Ev::default()
            },
        );
        Ok(EmergencyStopPlan {
            vm_id,
            run_id,
            report,
            terminated_sessions: terminated,
        })
    }

    /// Synchronous helper: Emergency Stop with termination via `backend.force_stop`.
    /// Completion is declared only if the end of the process is confirmed.
    pub fn emergency_stop_with<B: VmBackend>(
        &mut self,
        actor: Actor,
        vm_id: VmId,
        backend: &mut B,
    ) -> Result<(EmergencyStopPlan, Result<(), PublicError>), PublicError> {
        let plan = self.emergency_stop(actor, vm_id)?;
        let Some(run_id) = plan.run_id else {
            return Ok((plan, Ok(())));
        };
        let killed = backend.force_stop(vm_id, run_id);
        self.finish_emergency_stop(actor, vm_id, run_id, killed.clone())?;
        Ok((plan, killed))
    }

    /// Records the outcome of the termination of a run planned by
    /// [`Self::emergency_stop`]: `Ok` = end of the process observed (the VM
    /// becomes `stopped`), `Err` = not confirmed (the VM becomes `failed`).
    pub fn finish_emergency_stop(
        &mut self,
        actor: Actor,
        vm_id: VmId,
        run_id: RunId,
        killed: Result<(), PublicError>,
    ) -> Result<(), PublicError> {
        let outcome = match &killed {
            Ok(()) => {
                self.observe(
                    vm_id,
                    BackendObservation::ProcessExited {
                        run_id,
                        expected: true,
                    },
                )?;
                AuditOutcome::Succeeded
            }
            Err(_) => {
                self.set_state(vm_id, VmState::Failed);
                AuditOutcome::Failed
            }
        };
        self.emit_after(
            actor,
            AuditAction::EmergencyStop,
            outcome,
            Ev {
                vm_id: Some(vm_id),
                run_id: Some(run_id),
                error: killed.as_ref().err().map(|e| e.code),
                ..Ev::default()
            },
        );
        Ok(())
    }
}

fn agent_session(caller: &Caller) -> Option<SessionId> {
    match caller {
        Caller::Agent { session_id, .. } => Some(*session_id),
        Caller::User => None,
    }
}

fn rt_state(vms: &BTreeMap<VmId, VmRuntime>, vm_id: VmId) -> Option<VmState> {
    vms.get(&vm_id).map(|rt| rt.state)
}

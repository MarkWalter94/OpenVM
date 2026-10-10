//! Casi d'uso del core: catalogo, coda di mutazioni per VM, sessioni, Emergency
//! Stop, audit (ROADMAP 6.3, 7.3, 9; `docs/ARCHITECTURE.md` 4–6).
//!
//! Flusso di una richiesta:
//! 1. [`Core::submit`]: verifica al ricevimento (sessione, allowlist, policy
//!    effettiva, quote), idempotenza per `operation_id`, `run_id`, stato e
//!    occupazione della VM; poi accodamento.
//! 2. [`Core::begin_dispatch`]: nuova verifica **subito prima dell'inoltro**;
//!    se la sessione è revocata, scaduta o in pausa l'operazione è scartata senza
//!    effetto. Altrimenti l'operazione diventa `forwarded` e il supervisor chiama
//!    il backend senza trattenere il core.
//! 3. [`Core::finish_dispatch`] registra l'esito; [`Core::observe`] applica le
//!    osservazioni di processo e QMP, che prevalgono sullo stato persistito.
//!
//! L'Emergency Stop ([`Core::emergency_stop`]) non passa dalla coda: revoca le
//! sessioni, scarta la coda, identifica le richieste già inoltrate e chiede la
//! terminazione dell'esecuzione.

use std::collections::{BTreeMap, VecDeque};

use lightvm_proto::{
    Actor, Admission, AuditAction, AuditEvent, AuditOutcome, BackendObservation, Caller,
    Capability, DisplayGeometry, EffectivePolicy, ErrorCode, FileIdentity, Grant, HostCapacity,
    InputEvent, Label, Lease, ManagedRelPath, MediaEntry, MediaKind, MediaRecord, OperationId,
    OperationKind, OperationRecord, OperationStatus, PolicyLayer, PolicyRevision, PublicError,
    ResourceLimits, RevocationReport, RunEndReason, RunId, RunRecord, Screenshot, SessionId,
    SessionState, SessionToken, Sha256Digest, TerminationCause, UtcTimestamp, VmAction, VmConfig,
    VmId, VmRecord, VmRequest, VmState,
};

use crate::media::{check_media_kind, verify_media};
use crate::policy::{
    effective_policy, effective_resource_limits, effective_user_policy, host_layer,
};
use crate::ports::{AuditSink, CatalogStore, Clock, IdGenerator, MediaInspector, VmBackend};
use crate::resources::validate_config;
use crate::session::{SessionRegistry, SessionSpec, discard_code_for};

/// Limite rigido di operazioni in coda per VM, indipendente dalle quote.
pub const MAX_QUEUE_LEN: usize = 256;
/// Operazioni terminate ricordate per VM ai fini dell'idempotenza.
pub const IDEMPOTENCY_WINDOW: usize = 1024;

/// `true` se un'operazione del tipo dato è ammessa nello stato dato (guardia pura).
///
/// | Operazione | Stati ammessi |
/// | --- | --- |
/// | `start` | `stopped` |
/// | `stop` | `running` |
/// | `force_stop` | `starting`, `running`, `stopping` |
/// | `reset`, `screenshot`, `input` | `running` |
/// | `create`, `snapshot`, `restore` | `stopped` |
/// | `emergency_stop` | qualunque (percorso prioritario) |
#[must_use]
pub fn action_allowed_in(kind: OperationKind, state: VmState) -> bool {
    match kind {
        OperationKind::Start
        | OperationKind::Create
        | OperationKind::Snapshot
        | OperationKind::Restore => state == VmState::Stopped,
        OperationKind::Stop
        | OperationKind::Reset
        | OperationKind::Screenshot
        | OperationKind::Input => state == VmState::Running,
        OperationKind::ForceStop => state.has_process(),
        OperationKind::EmergencyStop => true,
    }
}

/// Configurazione del core.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CoreConfig {
    /// Limiti di risorse globali.
    pub global_limits: ResourceLimits,
    /// Livello di policy globale (capability e quote massime).
    pub global_policy: PolicyLayer,
    /// Capacità host rilevate dal probe.
    pub host: HostCapacity,
}

/// Stato di una VM come visto dal core.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VmStatus {
    /// VM.
    pub vm_id: VmId,
    /// Nome.
    pub name: lightvm_proto::VmName,
    /// Stato corrente (derivato dalle osservazioni).
    pub state: VmState,
    /// Esecuzione corrente.
    pub run_id: Option<RunId>,
    /// Configurazione corrente.
    pub config: VmConfig,
    /// Revisione della configurazione.
    pub config_revision: u64,
    /// Geometria corrente del display, se nota.
    pub geometry: Option<DisplayGeometry>,
    /// Operazioni in coda non inoltrate.
    pub queued: usize,
    /// Operazioni inoltrate in attesa di esito.
    pub in_flight: Vec<OperationId>,
}

/// Dati per l'avvio di un'esecuzione.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StartPlan {
    /// VM con configurazione corrente.
    pub vm: VmRecord,
    /// Voci del registro media da verificare con [`verify_media`] prima dell'uso.
    pub media: Vec<MediaEntry>,
}

/// Operazione pronta per l'inoltro al backend.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Dispatch {
    /// Operazione.
    pub operation_id: OperationId,
    /// VM.
    pub vm_id: VmId,
    /// Esecuzione di destinazione (nuova per `start`).
    pub run_id: RunId,
    /// Azione.
    pub action: VmAction,
    /// Presente solo per `start`.
    pub start: Option<StartPlan>,
    /// Dimensione massima ammessa per uno screenshot.
    pub max_screenshot_bytes: u64,
}

/// Esito positivo riportato dal backend.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DispatchOutcome {
    /// Operazione completata.
    Done,
    /// Screenshot prodotto.
    Screenshot(Screenshot),
}

/// Piano di Emergency Stop: il supervisor termina l'esecuzione con primitive OS
/// e conferma con [`Core::observe`] (`process_exited`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EmergencyStopPlan {
    /// VM.
    pub vm_id: VmId,
    /// Esecuzione da terminare, se esiste un processo.
    pub run_id: Option<RunId>,
    /// Coda scartata e richieste già inoltrate.
    pub report: RevocationReport,
    /// Sessioni terminate.
    pub terminated_sessions: Vec<SessionId>,
}

/// Esito della terminazione di una sessione.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionTermination {
    /// Sessione.
    pub session_id: SessionId,
    /// Causa.
    pub cause: TerminationCause,
    /// Coda scartata e richieste già inoltrate della sessione.
    pub report: RevocationReport,
    /// Emergency Stop richiesti (scadenza, perdita del client, chiusura UI...).
    pub emergency_stops: Vec<EmergencyStopPlan>,
}

/// Dati per registrare un supporto approvato (copia gestita già creata dallo storage).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MediaRegistration {
    /// Tipo.
    pub kind: MediaKind,
    /// SHA-256 della copia gestita.
    pub sha256: Sha256Digest,
    /// Dimensione in byte.
    pub size_bytes: u64,
    /// Origine dichiarata.
    pub declared_origin: Label,
    /// Percorso relativo della copia gestita.
    pub managed_path: ManagedRelPath,
    /// Identità OS della copia gestita.
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
        OperationKind::Snapshot | OperationKind::Restore => AuditAction::VmStateObserved,
        OperationKind::EmergencyStop => AuditAction::EmergencyStop,
    }
}

/// Bozza di evento di audit (il core assegna `seq` e `at`).
#[derive(Default)]
struct Ev {
    vm_id: Option<VmId>,
    run_id: Option<RunId>,
    operation_id: Option<OperationId>,
    session_id: Option<SessionId>,
    capability: Option<Capability>,
    error: Option<ErrorCode>,
    termination: Option<TerminationCause>,
}

/// Core del supervisor, generico sulle porte.
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
    op_index: BTreeMap<OperationId, VmId>,
    audit_seq: u64,
    audit_failures: u64,
}

impl<S, C, I, A> Core<S, C, I, A>
where
    S: CatalogStore,
    C: Clock,
    I: IdGenerator,
    A: AuditSink,
{
    /// Nuovo core. Chiamare [`Core::load_catalog`] all'avvio del supervisor.
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
        }
    }

    // ----- accessori -------------------------------------------------------

    /// Catalogo.
    pub fn store(&self) -> &S {
        &self.store
    }
    /// Destinazione dell'audit.
    pub fn audit_sink(&self) -> &A {
        &self.audit
    }
    /// Orologio.
    pub fn clock(&self) -> &C {
        &self.clock
    }
    /// Registro delle sessioni (sola lettura).
    pub fn sessions(&self) -> &SessionRegistry {
        &self.sessions
    }
    /// Revisione corrente della policy.
    pub fn policy_revision(&self) -> PolicyRevision {
        self.revision
    }
    /// Eventi di audit non registrati per errore della destinazione.
    pub fn audit_failures(&self) -> u64 {
        self.audit_failures
    }
    /// Configurazione.
    pub fn config(&self) -> &CoreConfig {
        &self.config
    }

    /// Aggiorna la capacità host (nuovo probe). Incrementa la revisione della policy.
    pub fn set_host_capacity(&mut self, host: HostCapacity) {
        self.config.host = host;
        self.revision = PolicyRevision(self.revision.0 + 1);
    }

    /// Imposta la policy di una VM. Incrementa la revisione della policy.
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
        };
        let r = self.audit.append(&event);
        if r.is_err() {
            self.audit_failures += 1;
        }
        r
    }

    /// Evento dopo un effetto già avvenuto: un errore della destinazione è contato, non propagato.
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

    // ----- autorizzazione --------------------------------------------------

    fn vm_layer(&self, vm_id: Option<VmId>) -> PolicyLayer {
        vm_id
            .and_then(|v| self.vm_policies.get(&v).cloned())
            .unwrap_or_else(PolicyLayer::permissive)
    }

    /// Verifica pura (nessun effetto) di una chiamata e policy effettiva risultante.
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
                PublicError::unsupported("capacità non disponibile su questo host")
                    .with_detail("capability", capability.as_str()),
            )
        } else {
            Err(
                PublicError::permission_denied("azione non autorizzata dalla policy effettiva")
                    .with_detail("capability", capability.as_str()),
            )
        }
    }

    fn effective_limits(&self) -> ResourceLimits {
        effective_resource_limits(&self.config.global_limits, None, &self.config.host)
    }

    // ----- catalogo --------------------------------------------------------

    /// Carica il catalogo all'avvio. Uno stato persistito con processo
    /// (`starting`/`running`/`stopping`) non è confermato da processo né QMP
    /// dopo un riavvio del supervisor: la VM entra in `recovering`.
    pub fn load_catalog(&mut self) -> Result<(), PublicError> {
        for record in self.store.list_vms()? {
            let state = if record.persisted_state.has_process() {
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

    /// Registra un supporto approvato nel registro media (azione dell'utente nell'app).
    pub fn register_media(&mut self, reg: MediaRegistration) -> Result<MediaRecord, PublicError> {
        if reg.size_bytes == 0 {
            return Err(PublicError::invalid_argument("supporto vuoto"));
        }
        if reg.size_bytes > reg.kind.max_bytes() {
            return Err(
                PublicError::resource_limit("supporto oltre la dimensione massima")
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
        };
        let entry = MediaEntry {
            record: record.clone(),
            managed_path: reg.managed_path,
            file_identity: reg.file_identity,
        };
        self.store.insert_media(&entry)?;
        Ok(record)
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
                    .ok_or_else(|| PublicError::not_found("supporto non presente nel registro"))?;
                check_media_kind(&entry, kind)?;
            }
        }
        Ok(())
    }

    /// Crea una VM nel catalogo. Solo l'utente: nessuna creazione da MCP nel MVP.
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
            let err =
                PublicError::permission_denied("creazione VM non disponibile alle sessioni agente");
            return Err(self.deny(actor, AuditAction::VmCreate, ev(), err));
        }
        let checks = validate_config(&config, &self.effective_limits())
            .and_then(|()| self.check_config_media(&config));
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

    /// Modifica la configurazione di una VM spenta (risorse e limiti,
    /// ADR-0017). Solo l'utente: le sessioni agente non modificano le VM in
    /// F1 (gli strumenti MCP, F5, resteranno entro i limiti dell'utente).
    /// Rifiuta VM non spente o con operazioni in corso (`state_conflict`),
    /// revisione diversa (`state_conflict`), disco ridotto
    /// (`invalid_argument`) e risorse oltre i limiti effettivi.
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
            let err = PublicError::permission_denied(
                "modifica della VM non disponibile alle sessioni agente",
            );
            return Err(self.deny(actor, AuditAction::VmConfigure, ev, err));
        }
        let Some(rt) = self.vms.get(&vm_id) else {
            let err = PublicError::not_found("VM inesistente");
            return Err(self.deny(actor, AuditAction::VmConfigure, ev, err));
        };
        let current = rt.record.clone();
        let check = if rt.state != VmState::Stopped || rt.lifecycle_pending() {
            Err(
                PublicError::state_conflict("la VM deve essere spenta e senza operazioni in corso")
                    .with_detail("state", rt.state.as_str()),
            )
        } else if current.config_revision != expected_revision {
            Err(
                PublicError::state_conflict("configurazione modificata nel frattempo")
                    .with_detail("revision", current.config_revision.to_string()),
            )
        } else if config.disk_gib < current.config.disk_gib {
            Err(PublicError::invalid_argument(
                "riduzione del disco non supportata: si può solo ingrandire",
            ))
        } else {
            validate_config(&config, &self.effective_limits())
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
        }
    }

    /// Elenco delle VM visibili al chiamante (filtrato dal grant per gli agenti).
    pub fn list_vms(&mut self, caller: &Caller) -> Result<Vec<VmStatus>, PublicError> {
        let now = self.clock.now();
        if let Err(err) = self.authorize(caller, None, Capability::VmList, now) {
            return Err(self.deny(
                caller.actor(),
                AuditAction::VmStateObserved,
                Ev::default(),
                err,
            ));
        }
        let allow = match caller {
            Caller::User => None,
            Caller::Agent { session_id, .. } => self
                .sessions
                .get(*session_id)
                .map(|s| s.grant.vm_allowlist.clone()),
        };
        Ok(self
            .vms
            .values()
            .filter(|rt| allow.as_ref().is_none_or(|a| a.contains(&rt.record.id)))
            .map(Self::status_of)
            .collect())
    }

    /// Stato di una VM, se il chiamante è autorizzato. Per un agente una VM fuori
    /// allowlist dà `permission_denied` anche se non esiste (nessun dato di altre VM).
    pub fn vm_status(&mut self, caller: &Caller, vm_id: VmId) -> Result<VmStatus, PublicError> {
        let now = self.clock.now();
        if let Err(err) = self.authorize(caller, Some(vm_id), Capability::VmStatus, now) {
            let ev = Ev {
                vm_id: Some(vm_id),
                capability: Some(Capability::VmStatus),
                ..Ev::default()
            };
            return Err(self.deny(caller.actor(), AuditAction::VmStateObserved, ev, err));
        }
        self.vms
            .get(&vm_id)
            .map(Self::status_of)
            .ok_or_else(|| PublicError::not_found("VM inesistente"))
    }

    /// Operazione per id: l'utente vede tutto, un agente solo le proprie.
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
            _ => Err(PublicError::not_found("operazione inesistente")),
        }
    }

    // ----- coda di mutazioni ----------------------------------------------

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
            return Err(PublicError::resource_limit(
                "numero massimo di VM in esecuzione raggiunto",
            )
            .with_detail("limit", limits.max_running_vms.to_string()));
        }
        let rt = self
            .vms
            .get(&vm_id)
            .ok_or_else(|| PublicError::not_found("VM inesistente"))?;
        // Limiti per VM ricalcolati all'avvio: la capacità host può essere cambiata
        // dopo la creazione (nuovo probe, riserva aumentata).
        validate_config(&rt.record.config, &limits)?;
        // Memoria guest complessiva delle VM con processo, più quella richiesta,
        // entro la memoria host meno la riserva. Le vCPU possono essere
        // sovrallocate tra VM diverse e non sono sommate.
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
                "memoria host insufficiente per le VM in esecuzione",
            )
            .with_detail("field", "memory_mib_total")
            .with_detail("requested", requested.to_string())
            .with_detail("limit", host_memory.to_string()));
        }
        Ok(())
    }

    /// Accetta una richiesta e la accoda (verifica al ricevimento).
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

        // 1. Autorizzazione (prima di rivelare l'esistenza della VM).
        let effective = match self.authorize(caller, Some(req.vm_id), capability, now) {
            Ok(e) => e,
            Err(err) => return Err(self.deny(actor, audit_action, ev(req.expected_run_id), err)),
        };
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
                // L'audit di accettazione precede l'effetto: se fallisce, nessun accodamento.
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
                    .ok_or_else(|| PublicError::not_found("VM inesistente"))?;
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

    /// Guardie di ammissione. `Ok(Some)` = duplicato idempotente; `Ok(None)` = da accodare.
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
            .ok_or_else(|| PublicError::not_found("VM inesistente"))?;

        // Idempotenza: stesso operation_id, stessa VM, stessa azione, stesso attore.
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
                    "operation_id già usato per una richiesta diversa",
                )),
            };
        }

        // Validazione strutturale e run_id.
        let needs_run = matches!(
            kind,
            OperationKind::Reset | OperationKind::Screenshot | OperationKind::Input
        );
        if needs_run && req.expected_run_id.is_none() {
            return Err(PublicError::invalid_argument(
                "expected_run_id obbligatorio per questa azione",
            ));
        }
        if let VmAction::Input(batch) = &req.action {
            batch.validate()?;
            if batch.vm_id != req.vm_id || Some(batch.run_id) != req.expected_run_id {
                return Err(PublicError::invalid_argument(
                    "vm_id o run_id del lotto diversi dalla richiesta",
                ));
            }
        }
        if let Some(expected) = req.expected_run_id
            && rt.run_id != Some(expected)
        {
            return Err(PublicError::state_conflict(
                "la richiesta si riferisce a un'esecuzione non corrente",
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
                    "geometria del display non corrente: acquisire uno screenshot",
                )
                .with_detail("reason", "stale_geometry"));
            }
        }

        // Stato.
        if !action_allowed_in(kind, rt.state) {
            return Err(
                PublicError::state_conflict("azione non ammessa nello stato corrente")
                    .with_detail("state", rt.state.as_str()),
            );
        }

        // Occupazione: una mutazione del ciclo di vita alla volta.
        if kind == OperationKind::ForceStop {
            if rt.force_stop_pending() {
                return Err(PublicError::vm_busy("arresto forzato già in corso"));
            }
        } else if rt.lifecycle_pending() {
            return Err(PublicError::vm_busy(
                "un'altra mutazione è in corso sulla VM",
            ));
        }

        // Limiti di coda e quote.
        let quotas = effective.quotas;
        let queue_cap = usize::try_from(quotas.max_queued_operations)
            .unwrap_or(usize::MAX)
            .min(MAX_QUEUE_LEN);
        if rt.queue.len() >= queue_cap {
            return Err(PublicError::resource_limit("coda della VM piena"));
        }
        if let Caller::Agent { session_id, .. } = caller
            && let Some(s) = self.sessions.get(*session_id)
        {
            let u = s.usage;
            let over = |used: u32, add: u32, max: u32| used.saturating_add(add) > max;
            if over(u.operations, 1, quotas.max_operations) {
                return Err(PublicError::resource_limit("quota di operazioni esaurita"));
            }
            if let VmAction::Input(batch) = &req.action {
                let n = u32::try_from(batch.events.len()).unwrap_or(u32::MAX);
                if over(u.input_events, n, quotas.max_input_events) {
                    return Err(PublicError::resource_limit("quota di input esaurita"));
                }
            }
            if kind == OperationKind::Screenshot && over(u.screenshots, 1, quotas.max_screenshots) {
                return Err(PublicError::resource_limit("quota di screenshot esaurita"));
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

    /// Chiude l'esecuzione corrente: stato `stopped` (o `failed`), coda scartata.
    fn end_run(&mut self, vm_id: VmId, final_state: VmState, default_reason: RunEndReason) {
        let now = self.clock.now();
        let Some(rt) = self.vms.get_mut(&vm_id) else {
            return;
        };
        let reason = rt.pending_end.take().unwrap_or(default_reason);
        let run = rt.run_id.take();
        rt.geometry = None;
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

    /// Prossima operazione da inoltrare per la VM, dopo la seconda verifica.
    /// `None` se la coda è vuota o un'altra operazione è già inoltrata (un
    /// arresto forzato può essere inoltrato anche mentre un'altra operazione è in
    /// corso, per non restare bloccato dietro uno shutdown che non termina).
    pub fn begin_dispatch(&mut self, vm_id: VmId) -> Result<Option<Dispatch>, PublicError> {
        loop {
            let now = self.clock.now();
            let rt = self
                .vms
                .get_mut(&vm_id)
                .ok_or_else(|| PublicError::not_found("VM inesistente"))?;
            let front_is_force = rt
                .queue
                .front()
                .is_some_and(|p| p.record.kind == OperationKind::ForceStop);
            if !rt.in_flight.is_empty() && !front_is_force {
                return Ok(None);
            }
            let Some(p) = rt.queue.pop_front() else {
                return Ok(None);
            };
            let kind = p.record.kind;
            let state = rt.state;
            let current_run = rt.run_id;

            // Seconda verifica, immediatamente prima dell'inoltro.
            let effective = match self.authorize(&p.caller, Some(vm_id), p.action.capability(), now)
            {
                Ok(e) => e,
                Err(err) => {
                    self.discard(vm_id, p, err.code);
                    continue;
                }
            };
            if p.record.run_id.is_some() && p.record.run_id != current_run {
                self.discard(vm_id, p, ErrorCode::StateConflict);
                continue;
            }
            if !action_allowed_in(kind, state) {
                self.discard(vm_id, p, ErrorCode::StateConflict);
                continue;
            }
            let mut start = None;
            let run_id = if kind == OperationKind::Start {
                if let Err(err) = self.check_start_resources(vm_id) {
                    self.discard(vm_id, p, err.code);
                    continue;
                }
                let vm = self
                    .vms
                    .get(&vm_id)
                    .map(|rt| rt.record.clone())
                    .ok_or_else(|| PublicError::not_found("VM inesistente"))?;
                let mut media = Vec::new();
                let mut missing = false;
                for id in [vm.config.install_media, vm.config.data_media]
                    .into_iter()
                    .flatten()
                {
                    match self.store.get_media(id)? {
                        Some(e) => media.push(e),
                        None => missing = true,
                    }
                }
                if missing {
                    self.discard(vm_id, p, ErrorCode::NotFound);
                    continue;
                }
                let run = self.ids.run_id();
                self.store.insert_run(&RunRecord {
                    id: run,
                    vm_id,
                    config_revision: vm.config_revision,
                    baseline_id: None,
                    started_at: now,
                    ended_at: None,
                    end_reason: None,
                })?;
                if let Some(rt) = self.vms.get_mut(&vm_id) {
                    rt.run_id = Some(run);
                    rt.geometry = None;
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
            self.store.upsert_operation(&p.record)?;
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

    /// Registra l'esito di un'operazione inoltrata.
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
            .ok_or_else(|| PublicError::not_found("VM inesistente"))?;
        let idx = rt
            .in_flight
            .iter()
            .position(|p| p.record.id == operation_id)
            .ok_or_else(|| PublicError::not_found("operazione non inoltrata"))?;
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
                        "screenshot non coerente con la richiesta",
                    ))
                }
            }
            (Ok(DispatchOutcome::Screenshot(_)), _) => Err(PublicError::infrastructure(
                "esito del backend non coerente con l'operazione",
            )),
            (Ok(DispatchOutcome::Done), OperationKind::Screenshot) => Err(
                PublicError::infrastructure("screenshot mancante nell'esito del backend"),
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
        self.store.upsert_operation(&p.record)?;
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
                    // Fine processo non confermata: la riconciliazione decide.
                    self.set_state(vm_id, VmState::Failed);
                }
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
        Ok(record)
    }

    /// Helper sincrono: inoltra la prossima operazione al backend e ne registra
    /// l'esito. Per `start` ispeziona e verifica i media prima dell'uso.
    pub fn execute_next<B: VmBackend, M: MediaInspector>(
        &mut self,
        vm_id: VmId,
        backend: &mut B,
        inspector: &mut M,
    ) -> Result<Option<OperationRecord>, PublicError> {
        let Some(d) = self.begin_dispatch(vm_id)? else {
            return Ok(None);
        };
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
                    (Ok(_), None) => Err(PublicError::infrastructure("piano di avvio mancante")),
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
                            "screenshot oltre il limite di byte",
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
            .map(Some)
    }

    // ----- osservazioni e riconciliazione ---------------------------------

    /// Applica un'osservazione del backend. Le osservazioni riferite a
    /// un'esecuzione non corrente sono ignorate (`Ok(false)`).
    pub fn observe(&mut self, vm_id: VmId, obs: BackendObservation) -> Result<bool, PublicError> {
        let rt = self
            .vms
            .get(&vm_id)
            .ok_or_else(|| PublicError::not_found("VM inesistente"))?;
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
                }
            }
        }
        Ok(true)
    }

    /// Registra la geometria corrente del display (per esempio dal client RFB).
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
            .ok_or_else(|| PublicError::not_found("VM inesistente"))?;
        if rt.run_id != Some(run_id) {
            return Ok(false);
        }
        rt.geometry = Some(geometry);
        Ok(true)
    }

    /// `failed → recovering`: avvio della riconciliazione.
    pub fn begin_recovery(&mut self, vm_id: VmId) -> Result<(), PublicError> {
        let state =
            rt_state(&self.vms, vm_id).ok_or_else(|| PublicError::not_found("VM inesistente"))?;
        if state == VmState::Recovering {
            return Ok(());
        }
        if !self.set_state(vm_id, VmState::Recovering) {
            return Err(PublicError::state_conflict(
                "riconciliazione non ammessa nello stato corrente",
            )
            .with_detail("state", state.as_str()));
        }
        Ok(())
    }

    /// Riconciliazione all'avvio di un supervisor nuovo, quando il chiamante
    /// garantisce che nessun processo di un'esecuzione precedente sia vivo
    /// (Windows: il Job Object del supervisor precedente è chiuso e i suoi
    /// processi terminati). Le VM in `recovering` senza intenti aperti nel
    /// journal tornano `stopped`; le altre restano in riconciliazione.
    /// Restituisce le VM riportate a `stopped`.
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

    /// `recovering → stopped`: riconciliazione completata dal supervisor
    /// (processi assenti verificati, journal applicato).
    pub fn finish_recovery(&mut self, vm_id: VmId) -> Result<(), PublicError> {
        let state =
            rt_state(&self.vms, vm_id).ok_or_else(|| PublicError::not_found("VM inesistente"))?;
        if state != VmState::Recovering {
            return Err(PublicError::state_conflict("VM non in riconciliazione"));
        }
        if let Some(rt) = self.vms.get_mut(&vm_id)
            && rt.run_id.is_some()
        {
            rt.pending_end = Some(RunEndReason::Reconciled);
        }
        self.end_run(vm_id, VmState::Stopped, RunEndReason::Reconciled);
        Ok(())
    }

    // ----- sessioni --------------------------------------------------------

    /// Crea una sessione agente approvata dall'utente e ne emette grant e lease.
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
                    "numero massimo di sessioni agente raggiunto",
                ));
            }
            if spec.expires_at <= now {
                return Err(PublicError::invalid_argument(
                    "scadenza del grant nel passato",
                ));
            }
            if spec.vm_allowlist.is_empty() {
                return Err(PublicError::invalid_argument("allowlist VM vuota"));
            }
            for vm in &spec.vm_allowlist {
                if !self.vms.contains_key(vm) {
                    return Err(PublicError::not_found("VM dell'allowlist inesistente"));
                }
            }
            Ok(())
        })();
        if let Err(err) = result {
            return Err(self.deny(Actor::User, AuditAction::SessionCreate, Ev::default(), err));
        }
        let session_id = self.ids.session_id();
        let lease = Lease::new(session_id, now, spec.lease_millis)?;
        let grant = Grant {
            id: self.ids.grant_id(),
            session_id,
            vm_allowlist: spec.vm_allowlist,
            capabilities: spec.capabilities,
            expires_at: spec.expires_at,
            quotas: spec.quotas,
            policy_revision: self.revision,
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

    /// Rinnova il lease (verificato dal supervisor, non dal client).
    pub fn renew_lease(
        &mut self,
        session_id: SessionId,
        token: &SessionToken,
        lease_millis: i64,
    ) -> Result<Lease, PublicError> {
        let now = self.clock.now();
        self.sessions.renew(session_id, token, lease_millis, now)
    }

    /// Pausa agente: blocca l'inoltro di nuove mutazioni (la VM continua).
    pub fn pause_session(&mut self, session_id: SessionId) -> Result<(), PublicError> {
        let s = self
            .sessions
            .get_mut(session_id)
            .ok_or_else(|| PublicError::not_found("sessione inesistente"))?;
        if s.state != SessionState::Active {
            return Err(PublicError::state_conflict("sessione non attiva"));
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

    /// Ripresa dopo una pausa (non dopo revoca o scadenza).
    pub fn resume_session(&mut self, session_id: SessionId) -> Result<(), PublicError> {
        let now = self.clock.now();
        let s = self
            .sessions
            .get_mut(session_id)
            .ok_or_else(|| PublicError::not_found("sessione inesistente"))?;
        if s.state != SessionState::Paused {
            return Err(PublicError::state_conflict("sessione non in pausa"));
        }
        if s.expiry_at(now).is_some() {
            return Err(PublicError::session_expired("sessione scaduta"));
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

    /// Revoca: invalida la sessione e scarta le sue operazioni non inoltrate. Le
    /// operazioni già inoltrate sono elencate, non annullate. Nessun arresto VM.
    pub fn revoke_session(
        &mut self,
        session_id: SessionId,
        cause: TerminationCause,
    ) -> Result<SessionTermination, PublicError> {
        if self.sessions.get(session_id).is_none() {
            return Err(PublicError::not_found("sessione inesistente"));
        }
        let report = self.revoke_inner(session_id, cause);
        Ok(SessionTermination {
            session_id,
            cause,
            report,
            emergency_stops: Vec::new(),
        })
    }

    /// Terminazione con arresto di emergenza delle VM della sessione: usata per
    /// scadenza, perdita del client, chiusura UI e ripresa da sospensione (ROADMAP 7.3).
    pub fn terminate_session_with_stop(
        &mut self,
        session_id: SessionId,
        cause: TerminationCause,
    ) -> Result<SessionTermination, PublicError> {
        let allow = self
            .sessions
            .get(session_id)
            .map(|s| s.grant.vm_allowlist.clone())
            .ok_or_else(|| PublicError::not_found("sessione inesistente"))?;
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

    /// Controllo periodico delle scadenze di grant e lease.
    pub fn tick(&mut self) -> Result<Vec<SessionTermination>, PublicError> {
        let now = self.clock.now();
        let due = self.sessions.due_expirations(now);
        due.into_iter()
            .map(|(id, cause)| self.terminate_session_with_stop(id, cause))
            .collect()
    }

    // ----- Emergency Stop --------------------------------------------------

    /// Emergency Stop: percorso prioritario fuori coda. Revoca le sessioni che
    /// controllano la VM, scarta l'intera coda, elenca le richieste già inoltrate
    /// e porta la VM in `stopping`. Il supervisor termina l'albero QEMU con
    /// primitive OS e conferma con `observe(process_exited)`: solo allora la VM è
    /// `stopped`.
    pub fn emergency_stop(
        &mut self,
        actor: Actor,
        vm_id: VmId,
    ) -> Result<EmergencyStopPlan, PublicError> {
        if !self.vms.contains_key(&vm_id) {
            return Err(PublicError::not_found("VM inesistente"));
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
            .ok_or_else(|| PublicError::not_found("VM inesistente"))?;
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
        // Operazioni rimaste (dell'utente o di sessioni non legate alla VM): la VM
        // va in arresto, quindi `state_conflict` come per la fine di un'esecuzione.
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

    /// Helper sincrono: Emergency Stop con terminazione tramite `backend.force_stop`.
    /// Il completamento è dichiarato solo se la fine del processo è confermata.
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
        Ok((plan, killed))
    }
}

fn rt_state(vms: &BTreeMap<VmId, VmRuntime>, vm_id: VmId) -> Option<VmState> {
    vms.get(&vm_id).map(|rt| rt.state)
}

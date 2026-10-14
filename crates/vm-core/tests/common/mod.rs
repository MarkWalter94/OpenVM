//! Fake in-memory implementations of the ports. No QEMU, no SQLite:
//! the tests that use them check logic and protocol, not the real boot.

#![allow(dead_code, unreachable_pub)]

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;

use vm_core::media::{ObservedMediaFacts, VerifiedMedia};
use vm_core::ports::{AuditSink, CatalogStore, Clock, IdGenerator, MediaInspector, VmBackend};
use vm_core::proto::*;
use vm_core::session::SessionSpec;
use vm_core::{Core, CoreConfig};

pub const T0: i64 = 1_760_000_000_000;

#[derive(Clone, Debug)]
pub struct FakeClock(pub Rc<Cell<i64>>);

impl FakeClock {
    pub fn advance(&self, ms: i64) {
        self.0.set(self.0.get() + ms);
    }
}

impl Clock for FakeClock {
    fn now(&self) -> UtcTimestamp {
        UtcTimestamp::from_unix_millis(self.0.get()).unwrap()
    }
}

#[derive(Debug)]
pub struct SeqIds {
    clock: Rc<Cell<i64>>,
    counter: u64,
}

impl IdGenerator for SeqIds {
    fn next_parts(&mut self) -> (u64, [u8; 10]) {
        self.counter += 1;
        let mut r = [0u8; 10];
        r[2..].copy_from_slice(&self.counter.to_be_bytes());
        (u64::try_from(self.clock.get()).unwrap(), r)
    }
}

#[derive(Debug, Default)]
pub struct MemStoreInner {
    pub vms: BTreeMap<VmId, VmRecord>,
    pub media: BTreeMap<MediaId, MediaEntry>,
    pub ops: BTreeMap<OperationId, OperationRecord>,
    pub runs: BTreeMap<RunId, RunRecord>,
    /// Sequence of persisted states, to check the transitions.
    pub state_log: Vec<(VmId, VmState)>,
    /// Makes `remove_vm`, `remove_media` and `import_media` fail (catalog
    /// error).
    pub fail_files: bool,
}

#[derive(Clone, Debug, Default)]
pub struct MemStore(pub Rc<RefCell<MemStoreInner>>);

impl CatalogStore for MemStore {
    fn insert_vm(&mut self, vm: &VmRecord) -> Result<(), PublicError> {
        self.0.borrow_mut().vms.insert(vm.id, vm.clone());
        Ok(())
    }
    fn get_vm(&self, id: VmId) -> Result<Option<VmRecord>, PublicError> {
        Ok(self.0.borrow().vms.get(&id).cloned())
    }
    fn update_vm(&mut self, vm: &VmRecord, expected_revision: u64) -> Result<(), PublicError> {
        let mut inner = self.0.borrow_mut();
        match inner.vms.get_mut(&vm.id) {
            Some(v) if v.config_revision == expected_revision => {
                *v = vm.clone();
                Ok(())
            }
            Some(_) => Err(PublicError::state_conflict("different revision")),
            None => Err(PublicError::not_found("VM does not exist")),
        }
    }
    fn list_vms(&self) -> Result<Vec<VmRecord>, PublicError> {
        Ok(self.0.borrow().vms.values().cloned().collect())
    }
    fn set_persisted_state(&mut self, id: VmId, state: VmState) -> Result<(), PublicError> {
        let mut inner = self.0.borrow_mut();
        if let Some(v) = inner.vms.get_mut(&id) {
            v.persisted_state = state;
        }
        inner.state_log.push((id, state));
        Ok(())
    }
    fn insert_media(&mut self, entry: &MediaEntry) -> Result<(), PublicError> {
        self.0
            .borrow_mut()
            .media
            .insert(entry.record.id, entry.clone());
        Ok(())
    }
    fn get_media(&self, id: MediaId) -> Result<Option<MediaEntry>, PublicError> {
        Ok(self.0.borrow().media.get(&id).cloned())
    }
    fn upsert_operation(&mut self, op: &OperationRecord) -> Result<(), PublicError> {
        self.0.borrow_mut().ops.insert(op.id, op.clone());
        Ok(())
    }
    fn get_operation(&self, id: OperationId) -> Result<Option<OperationRecord>, PublicError> {
        Ok(self.0.borrow().ops.get(&id).cloned())
    }
    fn insert_run(&mut self, run: &RunRecord) -> Result<(), PublicError> {
        self.0.borrow_mut().runs.insert(run.id, run.clone());
        Ok(())
    }
    fn finish_run(
        &mut self,
        id: RunId,
        ended_at: UtcTimestamp,
        reason: RunEndReason,
    ) -> Result<(), PublicError> {
        if let Some(r) = self.0.borrow_mut().runs.get_mut(&id) {
            r.ended_at = Some(ended_at);
            r.end_reason = Some(reason);
        }
        Ok(())
    }
    fn list_media(&self) -> Result<Vec<MediaRecord>, PublicError> {
        Ok(self
            .0
            .borrow()
            .media
            .values()
            .map(|e| e.record.clone())
            .collect())
    }
    fn import_media(
        &mut self,
        source: &std::path::Path,
        kind: MediaKind,
        declared_origin: &Label,
    ) -> Result<MediaEntry, PublicError> {
        if self.0.borrow().fail_files {
            return Err(PublicError::infrastructure("catalog error"));
        }
        let n = self.0.borrow().media.len() as u64;
        let mut r = [0x5a; 10];
        r[2..].copy_from_slice(&n.to_be_bytes());
        let entry = MediaEntry {
            record: MediaRecord {
                id: MediaId::from_unix_millis(1, r),
                kind,
                sha256: Sha256Digest::from_bytes([7; 32]),
                size_bytes: 1024,
                declared_origin: declared_origin.clone(),
                approved_at: UtcTimestamp::from_unix_millis(T0).unwrap(),
                detected_guest: None,
                location: Some(source.to_string_lossy().into_owned()),
                missing: false,
            },
            // ADR-0029: the file chosen by the user is used in place.
            location: MediaLocation::Referenced(source.to_path_buf()),
            file_identity: None,
        };
        self.insert_media(&entry)?;
        Ok(entry)
    }
    fn remove_media(&mut self, id: MediaId) -> Result<(), PublicError> {
        let mut inner = self.0.borrow_mut();
        if inner.fail_files {
            return Err(PublicError::infrastructure("catalog error"));
        }
        inner
            .media
            .remove(&id)
            .map(|_| ())
            .ok_or_else(|| PublicError::not_found("medium not found"))
    }
    fn remove_vm(&mut self, id: VmId) -> Result<(), PublicError> {
        let mut inner = self.0.borrow_mut();
        if inner.fail_files {
            return Err(PublicError::infrastructure("catalog error"));
        }
        inner.vms.remove(&id);
        inner.runs.retain(|_, r| r.vm_id != id);
        inner.ops.retain(|_, o| o.vm_id != id);
        Ok(())
    }
}

/// In-memory audit. With the second field set to `true` every write fails
/// (audit destination unavailable).
#[derive(Clone, Debug, Default)]
pub struct MemAudit(pub Rc<RefCell<Vec<AuditEvent>>>, pub Rc<Cell<bool>>);

impl AuditSink for MemAudit {
    fn append(&mut self, event: &AuditEvent) -> Result<(), PublicError> {
        if self.1.get() {
            return Err(PublicError::infrastructure("audit unavailable"));
        }
        self.0.borrow_mut().push(event.clone());
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Call {
    Start(VmId, RunId, Vec<MediaId>),
    Shutdown(VmId, RunId),
    ForceStop(VmId, RunId),
    Reset(VmId, RunId),
    Screenshot(VmId, RunId),
    Input(VmId, RunId, usize),
}

#[derive(Debug, Default)]
pub struct FakeBackend {
    pub calls: Vec<Call>,
    pub fail_next: Option<PublicError>,
    pub geometry: Option<DisplayGeometry>,
}

impl FakeBackend {
    fn result(&mut self) -> Result<(), PublicError> {
        match self.fail_next.take() {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }
    pub fn count(&self, f: impl Fn(&Call) -> bool) -> usize {
        self.calls.iter().filter(|c| f(c)).count()
    }
}

impl VmBackend for FakeBackend {
    fn start(
        &mut self,
        vm: &VmRecord,
        run_id: RunId,
        media: &[VerifiedMedia],
    ) -> Result<(), PublicError> {
        self.calls.push(Call::Start(
            vm.id,
            run_id,
            media.iter().map(|m| m.record().id).collect(),
        ));
        self.result()
    }
    fn shutdown(&mut self, vm_id: VmId, run_id: RunId) -> Result<(), PublicError> {
        self.calls.push(Call::Shutdown(vm_id, run_id));
        self.result()
    }
    fn force_stop(&mut self, vm_id: VmId, run_id: RunId) -> Result<(), PublicError> {
        self.calls.push(Call::ForceStop(vm_id, run_id));
        self.result()
    }
    fn reset(&mut self, vm_id: VmId, run_id: RunId) -> Result<(), PublicError> {
        self.calls.push(Call::Reset(vm_id, run_id));
        self.result()
    }
    fn screenshot(
        &mut self,
        vm_id: VmId,
        run_id: RunId,
        _max_bytes: u64,
    ) -> Result<Screenshot, PublicError> {
        self.calls.push(Call::Screenshot(vm_id, run_id));
        self.result()?;
        Ok(Screenshot {
            vm_id,
            run_id,
            geometry: self.geometry.unwrap_or(DisplayGeometry {
                width: 1280,
                height: 800,
            }),
            format: ImageFormat::Png,
            sha256: Sha256Digest::from_bytes([7; 32]),
            byte_len: 1000,
            captured_at: UtcTimestamp::from_unix_millis(T0).unwrap(),
        })
    }
    fn send_input(
        &mut self,
        vm_id: VmId,
        run_id: RunId,
        _geometry: DisplayGeometry,
        events: &[InputEvent],
    ) -> Result<(), PublicError> {
        self.calls.push(Call::Input(vm_id, run_id, events.len()));
        self.result()
    }
}

/// Inspector that returns the registered facts, except for simulated replacements.
#[derive(Debug, Default)]
pub struct FakeInspector {
    /// Replaced content (different hash).
    pub replaced: BTreeSet<MediaId>,
    /// Managed copy found writable.
    pub writable: BTreeSet<MediaId>,
    /// Different OS identity (file replaced with the same content).
    pub moved: BTreeSet<MediaId>,
    /// Inspection impossible (file missing or unreadable).
    pub unreadable: BTreeSet<MediaId>,
}

impl MediaInspector for FakeInspector {
    fn inspect(&mut self, entry: &MediaEntry) -> Result<ObservedMediaFacts, PublicError> {
        let id = entry.record.id;
        if self.unreadable.contains(&id) {
            return Err(PublicError::infrastructure("managed copy not readable"));
        }
        let mut sha = entry.record.sha256;
        if self.replaced.contains(&id) {
            sha = Sha256Digest::from_bytes([0xee; 32]);
        }
        let file_identity = if self.moved.contains(&id) {
            Some(FileIdentity {
                volume: 2,
                file_id: [8; 16],
            })
        } else {
            entry.file_identity
        };
        Ok(ObservedMediaFacts {
            sha256: sha,
            size_bytes: entry.record.size_bytes,
            file_identity,
            read_only: !self.writable.contains(&id),
        })
    }
}

pub type TestCore = Core<MemStore, FakeClock, SeqIds, MemAudit>;

pub struct Harness {
    pub core: TestCore,
    pub clock: FakeClock,
    pub store: MemStore,
    pub audit: MemAudit,
    pub backend: FakeBackend,
    pub inspector: FakeInspector,
    pub op_counter: u64,
}

pub fn host() -> HostCapacity {
    HostCapacity {
        logical_cpus: 24,
        total_memory_mib: 32 * 1024,
        reserved_memory_mib: 4 * 1024,
        free_storage_gib: 500,
        reserved_storage_gib: 50,
        accelerator_available: true,
        screenshot_available: true,
        input_available: true,
        baseline_available: false,
    }
}

pub fn config() -> CoreConfig {
    CoreConfig {
        global_limits: ResourceLimits::MVP_DEFAULT,
        global_policy: PolicyLayer::permissive(),
        host: host(),
    }
}

impl Harness {
    pub fn new() -> Self {
        Self::with_config(config())
    }

    pub fn with_config(cfg: CoreConfig) -> Self {
        Self::with_store(cfg, MemStore::default())
    }

    pub fn with_store(cfg: CoreConfig, store: MemStore) -> Self {
        let cell = Rc::new(Cell::new(T0));
        let clock = FakeClock(cell.clone());
        let audit = MemAudit::default();
        let mut core = Core::new(
            store.clone(),
            clock.clone(),
            SeqIds {
                clock: cell,
                counter: 0,
            },
            audit.clone(),
            cfg,
        );
        // The fake backend runs every guest whatever the build host; the
        // host rule itself is tested in `windows_guests.rs`.
        core.set_guest_support(vm_core::resources::any_guest);
        Self {
            core,
            clock,
            store,
            audit,
            backend: FakeBackend::default(),
            inspector: FakeInspector::default(),
            op_counter: 0,
        }
    }

    pub fn op(&mut self) -> OperationId {
        self.op_counter += 1;
        let mut r = [0xaa; 10];
        r[2..].copy_from_slice(&self.op_counter.to_be_bytes());
        OperationId::from_unix_millis(u64::try_from(T0).unwrap(), r)
    }

    pub fn install_media(&mut self) -> MediaId {
        self.media(MediaKind::InstallIso, "ubuntu.iso")
    }

    pub fn media(&mut self, kind: MediaKind, name: &str) -> MediaId {
        self.core
            .register_media(vm_core::MediaRegistration {
                kind,
                sha256: Sha256Digest::from_bytes([1; 32]),
                size_bytes: 6 * 1024 * 1024 * 1024,
                declared_origin: Label::parse("downloaded by the user").unwrap(),
                location: MediaLocation::Managed(
                    ManagedRelPath::parse(format!("media/{name}")).unwrap(),
                ),
                file_identity: Some(FileIdentity {
                    volume: 1,
                    file_id: [9; 16],
                }),
            })
            .unwrap()
            .id
    }

    pub fn vm_config(&self, name: &str, media: Option<MediaId>) -> VmConfig {
        VmConfig {
            name: VmName::parse(name).unwrap(),
            guest_os: GuestOs::UbuntuLtsX86_64,
            vcpus: 2,
            memory_mib: 4096,
            disk_gib: 40,
            network: NetworkProfile::Offline,
            install_media: media,
            data_media: None,
            firmware: Firmware::Uefi,
            limits: Default::default(),
        }
    }

    pub fn create_vm(&mut self, name: &str) -> VmId {
        let m = self.install_media();
        let cfg = self.vm_config(name, Some(m));
        self.core.create_vm(&Caller::User, cfg).unwrap().id
    }

    pub fn req(&mut self, vm: VmId, run: Option<RunId>, action: VmAction) -> VmRequest {
        VmRequest {
            operation_id: self.op(),
            vm_id: vm,
            expected_run_id: run,
            action,
        }
    }

    pub fn run_next(&mut self, vm: VmId) -> Option<OperationRecord> {
        self.core
            .execute_next(vm, &mut self.backend, &mut self.inspector)
            .unwrap()
    }

    pub fn drain(&mut self, vm: VmId) -> Vec<OperationRecord> {
        let mut out = Vec::new();
        while let Some(r) = self.run_next(vm) {
            out.push(r);
        }
        out
    }

    /// Starts the VM as the user and returns the new run_id.
    pub fn start(&mut self, vm: VmId) -> RunId {
        let r = self.req(vm, None, VmAction::Start);
        self.core.submit(&Caller::User, r).unwrap();
        let rec = self.run_next(vm).unwrap();
        assert_eq!(rec.status, OperationStatus::Succeeded, "{rec:?}");
        let st = self.core.vm_status(&Caller::User, vm).unwrap();
        assert_eq!(st.state, VmState::Running);
        st.run_id.unwrap()
    }

    pub fn stop(&mut self, vm: VmId) {
        let r = self.req(vm, None, VmAction::Stop);
        self.core.submit(&Caller::User, r).unwrap();
        let rec = self.run_next(vm).unwrap();
        assert_eq!(rec.status, OperationStatus::Succeeded, "{rec:?}");
    }

    pub fn session(&mut self, vms: &[VmId], caps: &[Capability], token: u8) -> (SessionId, Caller) {
        let spec = SessionSpec {
            vm_allowlist: vms.iter().copied().collect(),
            capabilities: caps.iter().copied().collect(),
            quotas: Quotas::UNBOUNDED,
            expires_at: UtcTimestamp::from_unix_millis(self.clock.0.get() + 3_600_000).unwrap(),
            lease_millis: 10_000,
        };
        let (grant, _lease) = self
            .core
            .create_session(spec, SessionToken::from_bytes([token; 32]))
            .unwrap();
        // Explicit handover of the input (F4): a session with `vm.input`
        // receives the control of its VMs, as the user would give it.
        if caps.contains(&Capability::VmInput) {
            for vm in vms {
                self.core
                    .set_input_owner(
                        Actor::User,
                        *vm,
                        ipc::InputOwner::Agent {
                            session_id: grant.session_id,
                        },
                    )
                    .unwrap();
            }
        }
        (
            grant.session_id,
            Caller::Agent {
                session_id: grant.session_id,
                token: Some(SessionToken::from_bytes([token; 32])),
            },
        )
    }

    pub fn screenshot_geometry(&mut self, vm: VmId, run: RunId) {
        let r = self.req(vm, Some(run), VmAction::Screenshot);
        self.core.submit(&Caller::User, r).unwrap();
        self.drain(vm);
    }

    pub fn input(&self, vm: VmId, run: RunId, n: usize) -> VmAction {
        VmAction::Input(InputBatch {
            vm_id: vm,
            run_id: run,
            geometry: DisplayGeometry {
                width: 1280,
                height: 800,
            },
            events: (0..n)
                .map(|i| {
                    InputEvent::PointerMove(PointerMove {
                        x: u16::try_from(i % 32767).unwrap(),
                        y: 1,
                    })
                })
                .collect(),
        })
    }

    pub fn audit_events(&self) -> Vec<AuditEvent> {
        self.audit.0.borrow().clone()
    }
}

pub const ALL_CAPS: [Capability; 10] = Capability::ALL;

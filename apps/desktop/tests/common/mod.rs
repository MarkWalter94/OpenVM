//! Test stand-in of the supervisor: the state is built by the test, the
//! commands the UI sends are recorded. No process, no IPC.

#![allow(dead_code, reason = "each test file uses a part of the helpers")]
#![allow(
    clippy::new_ret_no_self,
    reason = "returns the boxed supervisor and the test handles"
)]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use lightvm_desktop::service::{ServiceCommand, ServiceState, Supervisor, mark_pending};
use lightvm_proto::ipc::{ConsoleTicket, ParentLossPolicy, VmSummary};
use lightvm_proto::{
    HostCapacity, Label, MediaId, MediaKind, MediaRecord, RunId, Sha256Digest, UtcTimestamp, VmId,
    VmName, VmState,
};

/// Shared handles: the fake is moved into the app, the test keeps these.
#[derive(Clone, Default)]
pub(crate) struct Handles {
    pub(crate) state: Arc<Mutex<ServiceState>>,
    pub(crate) sent: Arc<Mutex<Vec<ServiceCommand>>>,
    pub(crate) ticket: Arc<Mutex<Option<ConsoleTicket>>>,
    pub(crate) stopped: Arc<Mutex<bool>>,
}

impl Handles {
    pub(crate) fn sent(&self) -> Vec<ServiceCommand> {
        self.sent.lock().unwrap().clone()
    }
    /// Recorded commands except the periodic ones (media list, ticket
    /// requests, input, VM list re-read while a VM is starting).
    pub(crate) fn actions(&self) -> Vec<ServiceCommand> {
        self.sent()
            .into_iter()
            .filter(|c| {
                !matches!(
                    c,
                    ServiceCommand::ListMedia
                        | ServiceCommand::Refresh
                        | ServiceCommand::OpenConsole(_)
                        | ServiceCommand::Input(_)
                )
            })
            .collect()
    }
    pub(crate) fn clear(&self) {
        self.sent.lock().unwrap().clear();
    }
    pub(crate) fn update(&self, f: impl FnOnce(&mut ServiceState)) {
        f(&mut self.state.lock().unwrap());
    }
}

pub(crate) struct FakeSupervisor {
    h: Handles,
}

impl FakeSupervisor {
    pub(crate) fn new(state: ServiceState) -> (Box<dyn Supervisor>, Handles) {
        let h = Handles::default();
        *h.state.lock().unwrap() = state;
        (Box::new(Self { h: h.clone() }), h)
    }
}

impl Supervisor for FakeSupervisor {
    fn pid(&self) -> u32 {
        4242
    }
    fn state(&self) -> ServiceState {
        self.h.state.lock().unwrap().clone()
    }
    fn send(&self, c: ServiceCommand) -> bool {
        mark_pending(&mut self.h.state.lock().unwrap(), &self.h.ticket, &c);
        self.h.sent.lock().unwrap().push(c);
        true
    }
    fn is_alive(&mut self) -> bool {
        !*self.h.stopped.lock().unwrap()
    }
    fn stop(&mut self, _timeout: Duration) -> (Option<i32>, bool) {
        *self.h.stopped.lock().unwrap() = true;
        (Some(0), false)
    }
    fn take_console_ticket(&self, vm_id: VmId) -> Option<ConsoleTicket> {
        let mut t = self.h.ticket.lock().unwrap();
        if t.as_ref().is_some_and(|t| t.vm_id == vm_id) {
            t.take()
        } else {
            None
        }
    }
    fn discard_console_ticket(&self) {
        *self.h.ticket.lock().unwrap() = None;
    }
}

pub(crate) fn host() -> HostCapacity {
    HostCapacity {
        logical_cpus: 8,
        total_memory_mib: 16384,
        reserved_memory_mib: 4096,
        free_storage_gib: 200,
        reserved_storage_gib: 20,
        accelerator_available: true,
        screenshot_available: true,
        input_available: true,
        baseline_available: false,
    }
}

/// Deterministic identifiers (snapshots).
pub(crate) fn vm_id(n: u8) -> VmId {
    VmId::from_unix_millis(1_760_000_000_000 + u64::from(n), [n; 10])
}

pub(crate) fn run_id(n: u8) -> RunId {
    RunId::from_unix_millis(1_760_000_100_000 + u64::from(n), [n; 10])
}

pub(crate) fn media_id(n: u8) -> MediaId {
    MediaId::from_unix_millis(1_760_000_200_000 + u64::from(n), [n; 10])
}

pub(crate) fn vm(n: u8, name: &str, state: VmState) -> VmSummary {
    VmSummary {
        vm_id: vm_id(n),
        name: VmName::parse(name).unwrap(),
        state,
        run_id: state.has_process().then(|| run_id(n)),
        config: lightvm_proto::VmConfig {
            name: VmName::parse(name).unwrap(),
            guest_os: lightvm_proto::GuestOs::UbuntuLtsX86_64,
            vcpus: 2,
            memory_mib: 4096,
            disk_gib: 32,
            network: lightvm_proto::NetworkProfile::Offline,
            install_media: None,
            data_media: None,
            firmware: lightvm_proto::Firmware::Uefi,
            limits: Default::default(),
        },
        config_revision: 3,
        disk_used_bytes: Some(5_637_144_576),
        baselines: Default::default(),
        start: Default::default(),
    }
}

pub(crate) fn media(n: u8, kind: MediaKind, origin: &str) -> MediaRecord {
    MediaRecord {
        id: media_id(n),
        kind,
        sha256: Sha256Digest::from_bytes([n.wrapping_mul(17); 32]),
        size_bytes: 3_213_064_192,
        declared_origin: Label::parse(origin).unwrap(),
        approved_at: UtcTimestamp::from_unix_millis(1_760_000_000_000 + i64::from(n)).unwrap(),
    }
}

pub(crate) fn connected(vms: Vec<VmSummary>) -> ServiceState {
    ServiceState {
        connection: "connected".into(),
        connected: true,
        backend: Some("qemu".into()),
        accelerator: Some("whpx".into()),
        host: Some(host()),
        vms,
        policy: Some(ParentLossPolicy::KillVms),
        media: lightvm_desktop::service::MediaState {
            loaded: true,
            records: vec![
                media(
                    1,
                    MediaKind::InstallIso,
                    "ubuntu-24.04.3-live-server-amd64.iso",
                ),
                media(2, MediaKind::DataIso, "tools.iso"),
            ],
            error: None,
        },
        ..ServiceState::default()
    }
}

/// Lets the console send the input still waiting (one batch every
/// `INPUT_INTERVAL`): steps the harness until nothing is pending.
pub(crate) fn settle<S>(
    h: &mut egui_kittest::Harness<'_, S>,
    app: impl Fn(&S) -> &lightvm_desktop::LightVmApp,
) {
    for _ in 0..200 {
        if !app(h.state()).console_input_pending() {
            return;
        }
        std::thread::sleep(Duration::from_millis(5));
        h.step();
    }
    panic!("console input still pending");
}

/// Text inputs of the whole tree.
pub(crate) fn text_fields<S>(h: &egui_kittest::Harness<'_, S>) -> usize {
    use egui::accesskit::Role;
    use egui_kittest::kittest::Queryable as _;
    h.query_all_by_role(Role::TextInput).count()
        + h.query_all_by_role(Role::MultilineTextInput).count()
        + h.query_all_by_role(Role::SearchInput).count()
        + h.query_all_by_role(Role::SpinButton).count()
}

pub(crate) fn deterministic_frame(w: usize, h: usize) -> egui::ColorImage {
    let mut px = Vec::with_capacity(w * h);
    for y in 0..h {
        for x in 0..w {
            px.push(if x % 32 == 0 || y % 32 == 0 {
                egui::Color32::from_gray(200)
            } else {
                egui::Color32::from_rgb((x * 255 / w) as u8, (y * 255 / h) as u8, 96)
            });
        }
    }
    egui::ColorImage::new([w, h], px)
}

pub(crate) fn baseline_id(n: u8) -> lightvm_proto::BaselineId {
    lightvm_proto::BaselineId::from_unix_millis(1_760_000_300_000 + u64::from(n), [n; 10])
}

/// Verified baseline of VM `vm`, not current, not in use: 3 GiB on the
/// host, 32 GiB disk, created 2026-10-10 08:00 UTC plus `n` hours.
pub(crate) fn baseline(n: u8, vm: u8, label: &str) -> lightvm_proto::ipc::BaselineSummary {
    lightvm_proto::ipc::BaselineSummary {
        baseline_id: baseline_id(n),
        vm_id: vm_id(vm),
        label: Label::parse(label).unwrap(),
        created_at: UtcTimestamp::from_unix_millis(1_791_619_200_000 + i64::from(n) * 3_600_000)
            .unwrap(),
        clean_shutdown: true,
        protected: false,
        derived_from: None,
        config_revision: 3,
        virtual_size_bytes: 32 << 30,
        size_bytes: 3 << 30,
        disk_sha256: Sha256Digest::from_bytes([n; 32]),
        firmware_sha256: Some(Sha256Digest::from_bytes([n.wrapping_add(1); 32])),
        current: false,
        in_use: false,
    }
}

/// Fixed instant of the agent tests: 2026-10-11 09:12 UTC.
pub(crate) fn now() -> UtcTimestamp {
    UtcTimestamp::from_unix_millis(1_791_709_920_000).unwrap()
}

/// `now()` plus `s` seconds.
pub(crate) fn at(s: i64) -> UtcTimestamp {
    now().checked_add_millis(s * 1000).unwrap()
}

pub(crate) fn session_id(n: u8) -> lightvm_proto::SessionId {
    lightvm_proto::SessionId::from_unix_millis(1_760_000_400_000 + u64::from(n), [n; 10])
}

/// Pending request of the agent `client` with the code `482-913`, asking
/// for VM 1, screen and input; expires 4:12 after `now()`.
pub(crate) fn session_request(id: u64, client: &str) -> lightvm_proto::ipc::SessionRequestInfo {
    use lightvm_proto::Capability;
    lightvm_proto::ipc::SessionRequestInfo {
        request_id: id,
        connection_id: 7,
        client_name: client.into(),
        peer_pid: Some(4321),
        purpose: "Check that the text editor saves a file in the home folder".into(),
        requested_vms: vec![vm_id(1)],
        requested_capabilities: [
            Capability::VmScreenshot,
            Capability::VmInput,
            Capability::VmReset,
        ]
        .into(),
        verification_code: "482-913".into(),
        requested_at: at(-48),
        expires_at: at(252),
    }
}

/// Live session `n` of `client` on VM `vm`, 52 minutes left, with usage.
pub(crate) fn session(
    n: u8,
    vm: u8,
    client: &str,
    state: lightvm_proto::SessionState,
    input: bool,
) -> lightvm_proto::ipc::SessionInfo {
    use lightvm_proto::Capability;
    lightvm_proto::ipc::SessionInfo {
        session_id: session_id(n),
        vm_id: vm_id(vm),
        connection_id: (state != lightvm_proto::SessionState::Terminated).then_some(7),
        client_name: client.into(),
        purpose: "Check that the text editor saves a file in the home folder".into(),
        state,
        termination: None,
        capabilities: [
            Capability::VmList,
            Capability::VmStatus,
            Capability::VmScreenshot,
            Capability::VmInput,
        ]
        .into(),
        quotas: lightvm_proto::ipc::AGENT_DEFAULT_QUOTAS,
        usage: lightvm_proto::ipc::SessionUsage {
            operations: 12,
            input_events: 340,
            screenshots: 4,
        },
        created_at: at(-8 * 60),
        expires_at: at(52 * 60),
        lease_expires_at: at(8),
        input_owner: input,
    }
}

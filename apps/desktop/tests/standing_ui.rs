//! Unattended access (standing grant, ADR-0039), whole app with a stand-in
//! supervisor: the creation form (every capability offered, none that can
//! lose work preselected, 1/3/7 days, summary of what changes and
//! confirmation), the live grant on the Agents page (Revoke, Confirm when
//! suspended, unusable pairing file), the indicator on every page, the
//! notice at start, Settings → Security, and the capabilities added to the
//! ordinary approval form. No process, no IPC.

mod common;

use common::{FakeSupervisor, at, connected, session, session_request, vm, vm_id};
use egui::accesskit::Role;
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT as _, Queryable as _};
use lightvm_desktop::agents::{INTEGRATIONS, OFFERED, STANDING_CHANGES};
use lightvm_desktop::service::{
    RequestStatus, ServiceCommand, ServiceState, StandingControl, StandingFile, StandingOp,
};
use lightvm_desktop::{LightVmApp, Section};
use lightvm_proto::ipc::AGENT_DEFAULT_QUOTAS;
use lightvm_proto::{
    Capability, SessionState, StandingGrant, StandingGrantId, StandingGrantSpec,
    StandingGrantState, ToolShare, VmState,
};

fn grant_id(n: u8) -> StandingGrantId {
    StandingGrantId::from_unix_millis(1_760_000_600_000 + u64::from(n), [n; 10])
}

/// Live grant on VM 1 ("lab") for `claude-code`, created an hour before
/// `now()`, expiring 7 days after its creation; 4 sessions approved.
fn grant(state: StandingGrantState) -> StandingGrant {
    StandingGrant {
        grant_id: grant_id(1),
        vm_id: vm_id(1),
        capabilities: [
            Capability::VmList,
            Capability::VmStatus,
            Capability::VmScreenshot,
        ]
        .into(),
        tools: Default::default(),
        network_endpoints: Vec::new(),
        quotas: AGENT_DEFAULT_QUOTAS,
        lease_ms: 10_000,
        give_input: false,
        client_name: Some("claude-code".into()),
        created_at: at(-3600),
        expires_at: at(7 * 86_400 - 3600),
        vm_config_revision: 3,
        state,
        sessions_approved: 4,
        last_session_at: Some(at(-600)),
    }
}

fn catalog() -> ServiceState {
    let mut lab = vm(1, "lab", VmState::Running);
    lab.config.tools.enabled = true;
    lab.config.tools.guest_exec = true;
    lab.config.tools.clipboard = true;
    lab.config.tools.files_transfer = true;
    lab.config.tools.ui_inspect = true;
    lab.config.tools.ui_act = true;
    lab.config.tools.shares = vec![ToolShare {
        name: "src".into(),
        root: r"D:\src".into(),
        writable: true,
    }];
    let mut s = connected(vec![lab, vm(2, "ubuntu-dev", VmState::Stopped)]);
    s.agents.access = Some(false);
    s.agents.sessions_loaded = true;
    s.agents.standing.loaded = true;
    s
}

fn with_grant(state: StandingGrantState) -> ServiceState {
    let mut s = catalog();
    s.agents.standing.grant = Some(grant(state));
    s.agents.standing.file = Some(StandingFile::Ready(
        r"C:\Users\u\AppData\Local\LightVM\pairing\pairing.json".into(),
    ));
    s
}

fn app(state: ServiceState) -> (LightVmApp, common::Handles) {
    let (fake, hs) = FakeSupervisor::new(state);
    let mut app = LightVmApp::new(false);
    app.set_supervisor(fake);
    app.set_fixed_time(Some(common::now()));
    (app, hs)
}

fn harness(app: LightVmApp) -> Harness<'static, LightVmApp> {
    let mut h = Harness::builder()
        .with_size(egui::vec2(1100.0, 1400.0))
        .build_ui_state(|ui, app: &mut LightVmApp| app.ui(ui), app);
    h.run_steps(4);
    h
}

fn button<'a>(h: &'a Harness<'_, LightVmApp>, label: &'a str) -> egui_kittest::Node<'a> {
    h.get_by_role_and_label(Role::Button, label)
}

fn enabled(h: &Harness<'_, LightVmApp>, label: &str) -> bool {
    !button(h, label).accesskit_node().is_disabled()
}

fn click(h: &mut Harness<'_, LightVmApp>, label: &str) {
    button(h, label).click_accesskit();
    h.run_steps(4);
}

fn tap(h: &mut Harness<'_, LightVmApp>, label: &str) {
    h.get_by_label(label).click_accesskit();
    h.run_steps(2);
}

fn checked(h: &Harness<'_, LightVmApp>, label: &str) -> bool {
    h.get_by_label(label).accesskit_node().toggled() == Some(egui::accesskit::Toggled::True)
}

fn goto(h: &mut Harness<'_, LightVmApp>, section: Section) {
    click(h, section.label());
}

/// Commands except the periodic re-reads.
fn actions(hs: &common::Handles) -> Vec<ServiceCommand> {
    hs.actions()
        .into_iter()
        .filter(|c| {
            !matches!(
                c,
                ServiceCommand::GetAgentAccess
                    | ServiceCommand::GetStandingGrant
                    | ServiceCommand::ListSessionRequests
                    | ServiceCommand::ListSessions
                    | ServiceCommand::ListSessionReports
                    | ServiceCommand::GetSessionReport(_)
                    | ServiceCommand::GetSessionReportEvents { .. }
            )
        })
        .collect()
}

/// Acknowledges the notice shown at start while a grant is live.
fn keep(h: &mut Harness<'_, LightVmApp>) {
    click(h, "Keep the unattended access");
    assert!(!h.state().dialog_open());
}

const INDICATOR: &str = "Unattended agent access active on lab until 2026-10-18 08:12 UTC";
const STANDING_STOP: &str = "Emergency Stop: stop all VMs and revoke the unattended access";

#[test]
fn creation_form_offers_every_capability_and_creates_only_after_the_summary() {
    let (a, hs) = app(catalog());
    let mut h = harness(a);
    // No grant: no notice, no indicator.
    assert!(!h.state().dialog_open());
    assert!(
        h.query_by_label_contains("Unattended agent access")
            .is_none()
    );
    goto(&mut h, Section::Agents);
    h.get_by_label("No unattended access.");
    click(&mut h, "Create unattended access");

    // Two VMs: none preselected; the review waits for it.
    assert!(!checked(&h, "lab"));
    assert!(!checked(&h, "ubuntu-dev"));
    assert!(!enabled(&h, "Review the unattended access"));
    tap(&mut h, "lab");

    // Every VM capability, snapshot, restore and logs included: only list,
    // status and screenshots ticked, never one that can lose work.
    for (c, text, dangerous) in OFFERED {
        let preselected = matches!(
            c,
            Capability::VmList | Capability::VmStatus | Capability::VmScreenshot
        );
        assert_eq!(checked(&h, text), preselected, "{text}");
        assert!(!(dangerous && checked(&h, text)), "{text}");
    }
    for c in [
        Capability::VmSnapshot,
        Capability::VmRestore,
        Capability::VmLogs,
    ] {
        assert!(OFFERED.iter().any(|(x, _, _)| *x == c), "{c} offered");
    }
    assert_eq!(h.query_all_by_label("Can lose unsaved work").count(), 3);
    // The guest Tools capabilities the VM settings allow, all unticked;
    // the shared folder; USB, which the VM settings do not allow.
    for (_, text) in INTEGRATIONS {
        assert!(!checked(&h, text), "{text}");
        assert!(
            !h.get_by_label(text).accesskit_node().is_disabled(),
            "{text}"
        );
    }
    assert!(!checked(&h, "Shared folder: src"));
    assert!(
        h.get_by_label("Use USB devices")
            .accesskit_node()
            .is_disabled()
    );
    // Every capability of the enumeration is represented in the form.
    for c in Capability::ALL {
        let offered = OFFERED.iter().any(|(x, _, _)| *x == c)
            || INTEGRATIONS.iter().any(|(x, _)| *x == c)
            || matches!(
                c,
                Capability::ShareRead
                    | Capability::ShareWrite
                    | Capability::Usb
                    | Capability::NetworkEndpoints
            );
        assert!(offered, "{c} not offered");
    }
    h.get_by_label("Network");
    // Lifetimes: 1, 3, 7 days, 1 day first.
    for d in ["1 day", "3 days", "7 days"] {
        h.get_by_label(d);
    }
    assert_eq!(common::text_fields(&h), 1, "only the expected client name");

    tap(&mut h, "7 days");
    tap(&mut h, "Read LightVM events and QEMU logs");
    tap(&mut h, "Create a baseline with the VM off");
    tap(&mut h, "Execute commands in the guest user session");
    tap(&mut h, "Shared folder: src");
    tap(&mut h, "Large");
    tap(&mut h, "5 s");
    let field = h.get_by_role(Role::TextInput);
    assert_eq!(
        field.accesskit_node().label().as_deref(),
        Some("Expected client name")
    );
    field.focus();
    h.run_steps(2);
    h.get_by_role(Role::TextInput).type_text("claude-code");
    h.run_steps(4);
    assert_eq!(
        h.get_by_role(Role::TextInput)
            .accesskit_node()
            .value()
            .as_deref(),
        Some("claude-code")
    );
    assert!(actions(&hs).is_empty(), "nothing sent before the summary");

    // Summary: what changes, then the confirmation.
    click(&mut h, "Review the unattended access");
    h.get_by_label("What changes compared with a session you approve");
    for c in STANDING_CHANGES {
        h.get_by_label(c);
    }
    h.get_by_label_contains("No approval click per session");
    h.get_by_label_contains("Survives LightVM restarts");
    h.get_by_label_contains("The VM stays running between sessions and without an agent");
    h.get_by_label_contains("suspended when the VM's settings change");
    h.get_by_label("2026-10-18 09:12 UTC (about 7 days)");
    h.get_by_label("Only \u{201c}claude-code\u{201d}");
    let create = "Create the unattended access";
    assert!(!enabled(&h, create), "not before the confirmation");
    let consent =
        "I understand: sessions on lab are approved without asking me until 2026-10-18 09:12 UTC";
    assert!(!checked(&h, consent));
    tap(&mut h, consent);
    h.run_steps(2);
    assert!(enabled(&h, create));
    click(&mut h, create);
    let expected = StandingGrantSpec {
        vm_id: vm_id(1),
        capabilities: [
            Capability::VmList,
            Capability::VmStatus,
            Capability::VmScreenshot,
            Capability::VmSnapshot,
            Capability::VmLogs,
            Capability::GuestExec,
            Capability::ShareRead,
        ]
        .into(),
        tools: lightvm_proto::ToolGrant {
            shares: [("src".to_owned(), false)].into(),
            usb_devices: Default::default(),
        },
        network_endpoints: Vec::new(),
        quotas: Some(lightvm_desktop::agents::QuotaPreset::Large.quotas()),
        lease_ms: Some(5_000),
        give_input: false,
        duration_s: lightvm_proto::MAX_STANDING_GRANT_S,
        client_name: Some("claude-code".into()),
    };
    assert_eq!(
        actions(&hs),
        [ServiceCommand::CreateStandingGrant(Box::new(expected))]
    );
    h.get_by_label("Waiting for the supervisor\u{2026}");
    assert!(!enabled(&h, create));

    // Refused (another grant live): the reason, the form stays.
    hs.update(|s| {
        s.agents.standing.control = Some(StandingControl {
            op: StandingOp::Create,
            status: RequestStatus::Failed(lightvm_desktop::service::Failure::coded(
                lightvm_proto::ErrorCode::StateConflict,
                "a standing grant is live",
            )),
        });
    });
    h.run_steps(4);
    h.get_by_label("Another unattended access is active: only one at a time. Revoke it first.");

    // Created: the form closes, the live grant is shown with the indicator.
    hs.update(|s| {
        s.agents.standing.control = Some(StandingControl {
            op: StandingOp::Create,
            status: RequestStatus::Done(()),
        });
        s.agents.standing.grant = Some(grant(StandingGrantState::Active));
        s.agents.standing.file = Some(StandingFile::Ready("pairing.json".into()));
    });
    h.run_steps(4);
    assert!(
        h.query_by_label("What changes compared with a session you approve")
            .is_none()
    );
    h.get_by_label(INDICATOR);
    // Created in this run: no notice.
    assert!(!h.state().dialog_open());
    assert!(h.query_by_label("Create unattended access").is_none());
}

#[test]
fn creation_can_be_cancelled_and_goes_back_to_the_choices() {
    let (a, hs) = app(catalog());
    let mut h = harness(a);
    goto(&mut h, Section::Agents);
    click(&mut h, "Create unattended access");
    tap(&mut h, "ubuntu-dev");
    click(&mut h, "Review the unattended access");
    click(&mut h, "Back to the unattended access choices");
    assert!(checked(&h, "ubuntu-dev"), "the choices are kept");
    click(&mut h, "Cancel the unattended access");
    h.get_by_label("No unattended access.");
    assert!(actions(&hs).is_empty());
}

#[test]
fn ordinary_approval_form_offers_snapshot_restore_and_logs() {
    let mut s = catalog();
    s.agents.access = Some(true);
    s.agents.requests = vec![session_request(3, "claude-desktop")];
    let (a, hs) = app(s);
    let mut h = harness(a);
    goto(&mut h, Section::Agents);
    click(&mut h, "Review the request of claude-desktop");
    for text in [
        "Create a baseline with the VM off",
        "Restore a baseline with the VM off (discards the changes made since)",
        "Read LightVM events and QEMU logs",
    ] {
        assert!(!checked(&h, text), "{text}");
    }
    assert_eq!(
        h.query_all_by_label("Can lose unsaved work").count(),
        3,
        "stop, reset and restore marked"
    );
    tap(&mut h, "Create a baseline with the VM off");
    tap(
        &mut h,
        "Restore a baseline with the VM off (discards the changes made since)",
    );
    tap(&mut h, "Read LightVM events and QEMU logs");
    tap(&mut h, "The agent shows the code 482-913");
    h.run_steps(4);
    click(&mut h, "Approve the session of claude-desktop");
    let approvals: Vec<_> = actions(&hs)
        .into_iter()
        .filter_map(|c| match c {
            ServiceCommand::ApproveSession { approval, .. } => Some(approval),
            _ => None,
        })
        .collect();
    assert_eq!(approvals.len(), 1);
    for c in [
        Capability::VmSnapshot,
        Capability::VmRestore,
        Capability::VmLogs,
    ] {
        assert!(approvals[0].capabilities.contains(&c), "{c}");
    }
}

#[test]
fn notice_at_start_must_be_acknowledged() {
    let (a, hs) = app(with_grant(StandingGrantState::Active));
    let mut h = harness(a);
    assert!(h.state().dialog_open());
    h.get_by_label("Unattended agent access is active");
    h.get_by_label_contains("An agent can use lab without asking you until 2026-10-18 08:12 UTC");
    h.get_by_label_contains("Sessions approved so far: 4.");
    // Esc does not dismiss it.
    h.key_press(egui::Key::Escape);
    h.run_steps(4);
    assert!(h.state().dialog_open());
    keep(&mut h);
    h.run_steps(4);
    assert!(
        h.query_by_label("Unattended agent access is active")
            .is_none()
    );
    assert!(actions(&hs).is_empty());

    // Revoke from the notice: the confirmation first.
    let (a, hs) = app(with_grant(StandingGrantState::Active));
    let mut h = harness(a);
    click(&mut h, "Revoke the unattended access now");
    assert!(h.state().dialog_open());
    h.get_by_label("Revoke the unattended access to lab?");
    click(&mut h, "Revoke unattended access");
    assert_eq!(
        actions(&hs),
        [ServiceCommand::RevokeStandingGrant(grant_id(1))]
    );
}

#[test]
fn no_notice_before_the_grant_is_read() {
    let mut s = with_grant(StandingGrantState::Active);
    s.agents.standing.loaded = false;
    s.agents.standing.grant = None;
    let (a, hs) = app(s);
    let mut h = harness(a);
    assert!(!h.state().dialog_open());
    // Read now, with a live grant: the notice opens.
    hs.update(|s| {
        s.agents.standing.loaded = true;
        s.agents.standing.grant = Some(grant(StandingGrantState::Active));
    });
    h.run_steps(4);
    assert!(h.state().dialog_open());
    h.get_by_label("Unattended agent access is active");
}

#[test]
fn indicator_on_every_page_with_revoke_and_emergency_stop() {
    let mut s = with_grant(StandingGrantState::Active);
    // A session approved by the grant is live too: both bars.
    let mut live = session(1, 1, "claude-code", SessionState::Active, false);
    live.standing_grant_id = Some(grant_id(1));
    s.agents.sessions = vec![live];
    let (a, hs) = app(s);
    let mut h = harness(a);
    keep(&mut h);
    for section in [
        Section::Dashboard,
        Section::Console,
        Section::Agents,
        Section::Settings,
    ] {
        goto(&mut h, section);
        h.get_by_label(INDICATOR);
        button(&h, STANDING_STOP);
        button(&h, "Revoke the unattended agent access");
        // The orange session bar stays, with its own Emergency Stop.
        button(&h, "Emergency Stop: stop all VMs and agent sessions");
    }
    goto(&mut h, Section::Agents);
    h.get_by_label("Approved automatically");

    click(&mut h, STANDING_STOP);
    assert_eq!(actions(&hs), [ServiceCommand::EmergencyStop]);
    hs.clear();

    // Revoke from the indicator: confirmation, Cancel sends nothing.
    click(&mut h, "Revoke the unattended agent access");
    h.get_by_label("Revoke the unattended access to lab?");
    h.get_by_label_contains("The VM keeps running.");
    click(&mut h, "Cancel");
    assert!(actions(&hs).is_empty());
    click(&mut h, "Revoke the unattended agent access");
    click(&mut h, "Revoke unattended access");
    assert_eq!(
        actions(&hs),
        [ServiceCommand::RevokeStandingGrant(grant_id(1))]
    );
    h.get_by_label("Waiting for the supervisor…");
    // Revoked: the dialog closes, the indicator goes.
    hs.update(|s| {
        let g = s.agents.standing.grant.take().unwrap();
        s.agents.standing.ended = Some(StandingGrant {
            state: StandingGrantState::Revoked {
                cause: lightvm_proto::StandingRevokeCause::User,
                at: at(0),
            },
            ..g
        });
        s.agents.standing.control = Some(StandingControl {
            op: StandingOp::Revoke(grant_id(1)),
            status: RequestStatus::Done(()),
        });
    });
    h.run_steps(4);
    assert!(!h.state().dialog_open());
    assert!(h.query_by_label(INDICATOR).is_none());
    h.get_by_label("No unattended access.");
    h.get_by_label("The last unattended access, on lab, was revoked on 2026-10-11 09:12 UTC.");
    assert!(enabled(&h, "Create unattended access"));
}

#[test]
fn agents_page_shows_the_live_grant() {
    let (a, _hs) = app(with_grant(StandingGrantState::Active));
    let mut h = harness(a);
    keep(&mut h);
    goto(&mut h, Section::Agents);
    h.get_by_label("Active");
    h.get_by_label("Only \u{201c}claude-code\u{201d} (name declared by the client)");
    h.get_by_label("2026-10-18 08:12 UTC \u{b7} 6 d 23 h left");
    h.get_by_label("4");
    h.get_by_label("Last session approved on 2026-10-11 09:02 UTC");
    h.get_by_label_contains("See the VM in its list");
    h.get_by_label("No network: the VM runs offline in its sessions.");
    assert!(h.query_by_label("Create unattended access").is_none());
    assert!(h.query_by_label("Confirm the unattended access").is_none());
    assert!(enabled(&h, "Revoke the unattended access"));
    // Ordinary pairing would replace its file: not available.
    assert!(!enabled(&h, "Pair a client"));
    h.get_by_label_contains("the unattended access below works whether it is on or off");
}

#[test]
fn suspended_grant_explains_why_and_is_confirmed_again() {
    let (a, hs) = app(with_grant(StandingGrantState::Suspended));
    let mut h = harness(a);
    keep(&mut h);
    h.get_by_label(
        "Unattended agent access suspended on lab (VM configuration changed) until 2026-10-18 08:12 UTC",
    );
    goto(&mut h, Section::Agents);
    h.get_by_label("Suspended");
    h.get_by_label_contains("VM configuration changed \u{2014} Confirm");
    click(&mut h, "Confirm the unattended access");
    assert_eq!(
        actions(&hs),
        [ServiceCommand::ConfirmStandingGrant(grant_id(1))]
    );
    // Revoke waits while the confirmation is pending.
    assert!(!enabled(&h, "Revoke the unattended access"));
    hs.clear();
    hs.update(|s| {
        s.agents.standing.control = Some(StandingControl {
            op: StandingOp::Confirm(grant_id(1)),
            status: RequestStatus::Done(()),
        });
    });
    h.run_steps(4);
    click(&mut h, "Revoke the unattended access");
    click(&mut h, "Revoke unattended access");
    assert_eq!(
        actions(&hs),
        [ServiceCommand::RevokeStandingGrant(grant_id(1))]
    );
}

#[test]
fn missing_pairing_file_makes_the_grant_unusable() {
    let mut s = with_grant(StandingGrantState::Active);
    s.agents.standing.file = Some(StandingFile::Unusable("pairing file not found".into()));
    let (a, _hs) = app(s);
    let mut h = harness(a);
    h.get_by_label_contains("Its pairing file is missing or unusable (pairing file not found)");
    keep(&mut h);
    h.get_by_label(&format!(
        "{INDICATOR} \u{b7} pairing file missing: the agent cannot connect"
    ));
    goto(&mut h, Section::Agents);
    h.get_by_label(
        "The pairing file of the unattended access is missing or unusable (pairing file not found): the agent cannot connect. Revoke it and create it again.",
    );
}

#[test]
fn settings_security_states_the_unattended_access() {
    let (a, _hs) = app(catalog());
    let mut h = harness(a);
    goto(&mut h, Section::Settings);
    h.get_by_label_contains("Off: every agent session needs your approval.");
    assert!(h.query_by_label("Not available in this version.").is_none());

    let (a, _hs) = app(with_grant(StandingGrantState::Active));
    let mut h = harness(a);
    keep(&mut h);
    goto(&mut h, Section::Settings);
    h.get_by_label_contains(
        "On lab until 2026-10-18 08:12 UTC: its agent's sessions are approved without asking you.",
    );
    h.get_by_label_contains("AI Workspace page, Unattended access");
}

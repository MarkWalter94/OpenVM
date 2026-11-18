//! Agents page and agent session indicator (F4), whole app with a stand-in
//! supervisor: agent access off by default, approval with the verification
//! code (one VM, capabilities, duration, limits, lease, input), denial,
//! pause/resume/revocation, input ownership in the console, the indicator
//! on every page (full screen console included) with the Emergency Stop,
//! the session page. No process, no IPC.

mod common;

use common::{FakeSupervisor, at, connected, session, session_id, session_request, vm, vm_id};
use egui::accesskit::Role;
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT as _, Queryable as _};
use lightvm_desktop::console::{ConsoleTarget, LiveConsole};
use lightvm_desktop::service::{
    AgentControl, AgentOp, Approval, RequestStatus, ServiceCommand, ServiceState,
};
use lightvm_desktop::{LightVmApp, Section};
use lightvm_proto::ipc::{InputOwner, SessionApproval};
use lightvm_proto::{Capability, InputEvent, SessionState, TerminationCause, VmState};

const CLIENT: &str = "claude-desktop";

fn catalog() -> ServiceState {
    connected(vec![
        vm(1, "lab", VmState::Running),
        vm(2, "ubuntu-dev", VmState::Stopped),
    ])
}

fn with_request(access: bool) -> ServiceState {
    let mut s = catalog();
    s.agents.access = Some(access);
    s.agents.sessions_loaded = true;
    s.agents.requests = vec![session_request(3, CLIENT)];
    s
}

fn with_session(state: SessionState, input: bool) -> ServiceState {
    let mut s = catalog();
    s.agents.access = Some(true);
    s.agents.sessions_loaded = true;
    s.agents.sessions = vec![session(1, 1, CLIENT, state, input)];
    if input {
        s.agents.input_owners.insert(
            vm_id(1),
            InputOwner::Agent {
                session_id: session_id(1),
            },
        );
    }
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
        .with_size(egui::vec2(1100.0, 860.0))
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

/// Clicks a button through AccessKit, as assistive technology does: also
/// below the fold of the long approval form, and never on a tooltip left
/// by the previous click.
fn click(h: &mut Harness<'_, LightVmApp>, label: &str) {
    button(h, label).click_accesskit();
    h.run_steps(4);
}

/// Clicks any control (checkbox, radio, choice) by label through AccessKit.
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
                    | ServiceCommand::ListSessionRequests
                    | ServiceCommand::ListSessions
                    | ServiceCommand::ListSessionReports
                    | ServiceCommand::GetSessionReport(_)
                    | ServiceCommand::GetSessionReportEvents { .. }
            )
        })
        .collect()
}

const BANNER_STOP: &str = "Emergency Stop: stop all VMs and agent sessions";

#[test]
fn agent_access_is_off_by_default_and_explains_what_it_allows() {
    let mut s = catalog();
    s.agents.access = Some(false);
    let (app, hs) = app(s);
    let mut h = harness(app);
    goto(&mut h, Section::Agents);
    assert!(!checked(&h, "Allow agents to request sessions"));
    h.get_by_label_contains("Off every time LightVM starts");
    h.get_by_label_contains("nothing is granted until you approve a request");
    h.get_by_label("Off: agents cannot ask for sessions");
    // Pairing needs agent access.
    assert!(!enabled(&h, "Pair a client"));
    h.get_by_label("No session requests.");
    h.get_by_label("No agent session is active.");
    // No text field, no indicator without sessions or requests.
    assert_eq!(common::text_fields(&h), 0);
    assert!(h.query_by_label(BANNER_STOP).is_none());

    tap(&mut h, "Allow agents to request sessions");
    h.run_steps(4);
    assert_eq!(actions(&hs), [ServiceCommand::SetAgentAccess(true)]);
    hs.update(|s| s.agents.access = Some(true));
    h.run_steps(4);
    assert!(checked(&h, "Allow agents to request sessions"));
    assert!(enabled(&h, "Pair a client"));

    // Unknown state (not read yet): the switch waits.
    hs.update(|s| s.agents.access = None);
    h.run_steps(4);
    assert!(
        h.get_by_label("Allow agents to request sessions")
            .accesskit_node()
            .is_disabled()
    );
}

#[test]
fn pairing_writes_the_file_and_never_shows_the_credential() {
    let mut s = catalog();
    s.agents.access = Some(true);
    let (app, hs) = app(s);
    let mut h = harness(app);
    goto(&mut h, Section::Agents);
    click(&mut h, "Pair a client");
    assert!(h.state().dialog_open());
    h.get_by_label("Pair an agent client");
    h.get_by_label_contains("it gives no access to any VM");
    click(&mut h, "Write pairing file");
    assert_eq!(actions(&hs), [ServiceCommand::CreatePairing]);
    h.get_by_label("Waiting for the supervisor…");
    let path = std::path::PathBuf::from(r"C:\Users\u\AppData\Local\LightVM\pairing\pairing.json");
    hs.update(|s| {
        s.agents.pairing = RequestStatus::Done(lightvm_desktop::pairing::PairingFile {
            path: path.clone(),
            expires_at: at(600),
        });
    });
    h.run_steps(4);
    h.get_by_label("Pairing file ready");
    h.get_by_label_contains(r"pairing\pairing.json, readable only by your user account");
    h.get_by_label_contains("valid until 09:22:00 UTC (10 min left)");
    click(&mut h, "Done");
    assert!(!h.state().dialog_open());
    h.get_by_label_contains("Pairing file ready until 09:22:00 UTC");
}

#[test]
fn request_is_approved_with_the_code_one_vm_and_the_chosen_limits() {
    let (app, hs) = app(with_request(true));
    let mut h = harness(app);
    // Indicator on the Dashboard: the request and its code.
    h.get_by_label_contains("asks for a session \u{b7} code 482-913");
    click(&mut h, "Review the session requests");
    assert_eq!(h.state().section(), Section::Agents);
    // The request: client, purpose as text, what it asks for, code large.
    h.get_by_label(CLIENT);
    h.get_by_label("Check that the text editor saves a file in the home folder");
    h.get_by_label_contains("Asks for lab:");
    h.get_by_label("Verification code 482-913");
    h.get_by_label("expires in 4:12");

    click(&mut h, &format!("Review the request of {CLIENT}"));
    assert_eq!(h.state().reviewing(), Some(3));
    let approve = format!("Approve the session of {CLIENT}");
    // The VM asked for is preselected; the code is not confirmed yet.
    assert!(!enabled(&h, &approve));
    assert!(checked(&h, "lab"));
    assert!(!checked(&h, "ubuntu-dev"));
    // Requested: screen and input yes, reset not (it can lose work).
    assert!(checked(&h, "See the screen (screenshots)"));
    assert!(checked(
        &h,
        "Type and use the mouse, only while it has keyboard and mouse control"
    ));
    assert!(!checked(&h, "Reset the VM"));
    assert!(!checked(&h, "Stop or force-stop the VM"));
    assert_eq!(
        h.query_all_by_label("Can lose unsaved work").count(),
        3,
        "stop, reset and baseline restore marked"
    );
    assert_eq!(common::text_fields(&h), 0);

    // Choices: other VM, reset too, input now, 4 hours, small limits, 5 s.
    tap(&mut h, "ubuntu-dev");
    tap(&mut h, "Reset the VM");
    tap(&mut h, "Give keyboard and mouse to the agent now");
    tap(&mut h, "4 hours");
    tap(&mut h, "Small");
    tap(&mut h, "5 s");
    h.run_steps(4);
    h.get_by_label("1,000 operations, 20,000 input events, 200 screenshots");
    assert!(!enabled(&h, &approve), "code not confirmed");
    tap(&mut h, "The agent shows the code 482-913");
    h.run_steps(4);
    assert!(enabled(&h, &approve));
    click(&mut h, &approve);
    let expected = SessionApproval {
        tools: Default::default(),
        vm_id: vm_id(2),
        capabilities: [
            Capability::VmList,
            Capability::VmStatus,
            Capability::VmReset,
            Capability::VmScreenshot,
            Capability::VmInput,
        ]
        .into(),
        duration_s: 4 * 3600,
        quotas: Some(lightvm_desktop::agents::QuotaPreset::Small.quotas()),
        lease_ms: Some(5_000),
        give_input: true,
        network_endpoints: Vec::new(),
    };
    assert_eq!(
        actions(&hs),
        [ServiceCommand::ApproveSession {
            request_id: 3,
            approval: expected,
        }]
    );
    h.get_by_label("Waiting for the supervisor…");
    assert!(!enabled(&h, &approve));

    // Refused (another session): the form stays, with the reason.
    hs.update(|s| {
        s.agents.approval = Some(Approval {
            request_id: 3,
            status: RequestStatus::Failed(lightvm_desktop::service::Failure::coded(
                lightvm_proto::ErrorCode::ResourceLimit,
                "one live session",
            )),
        });
    });
    h.run_steps(4);
    h.get_by_label("Another agent session is active: only one at a time. End it first.");
    assert!(enabled(&h, &approve));

    // Approved: the form closes, the session is listed.
    hs.update(|s| {
        s.agents.approval = Some(Approval {
            request_id: 3,
            status: RequestStatus::Done(session_id(1)),
        });
        s.agents.requests.clear();
        s.agents.sessions = vec![session(1, 2, CLIENT, SessionState::Active, true)];
    });
    h.run_steps(4);
    assert_eq!(h.state().reviewing(), None);
    h.get_by_label("No session requests.");
    h.get_by_label("VM ubuntu-dev \u{b7} 52 min left");
    h.get_by_label(
        "12 of 10,000 operations \u{b7} 340 of 200,000 input events \u{b7} 4 of 2,000 screenshots",
    );
}

/// ADR-0033: the endpoints of the chosen VM are offered unticked; ticking
/// one grants `network.endpoints` with exactly that endpoint; the request's
/// hint is shown; a NAT VM that runs is flagged.
#[test]
fn network_endpoints_are_granted_only_when_ticked() {
    let mut s = with_request(true);
    s.agents.requests[0].requested_endpoints = vec!["sql.lan".into()];
    s.vms[0].config.network = lightvm_proto::NetworkProfile::Endpoints;
    s.vms[0].config.endpoints = vec![
        lightvm_proto::NetworkEndpoint {
            name: "sql.lan".into(),
            host: "sqlserver.corp.example".into(),
            port: 1433,
            virtual_ip: std::net::Ipv4Addr::new(10, 0, 2, 100),
        },
        lightvm_proto::NetworkEndpoint {
            name: "wcf".into(),
            host: "127.0.0.1".into(),
            port: 8733,
            virtual_ip: std::net::Ipv4Addr::new(10, 0, 2, 101),
        },
    ];
    let (a1, hs) = app(s);
    let mut h = harness(a1);
    goto(&mut h, Section::Agents);
    h.get_by_label_contains("Network endpoints asked for: sql.lan.");
    click(&mut h, &format!("Review the request of {CLIENT}"));
    let sql = "sql.lan \u{2192} sqlserver.corp.example:1433 (inside the guest 10.0.2.100:1433)";
    let wcf = "wcf \u{2192} 127.0.0.1:8733 (inside the guest 10.0.2.101:8733)";
    assert!(!checked(&h, sql), "never ticked by default");
    assert!(!checked(&h, wcf));
    tap(&mut h, sql);
    h.get_by_label("An allowed endpoint receives whatever the guest sends to it.");
    tap(&mut h, "The agent shows the code 482-913");
    h.run_steps(4);
    click(&mut h, &format!("Approve the session of {CLIENT}"));
    let sent: Vec<SessionApproval> = actions(&hs)
        .into_iter()
        .filter_map(|c| match c {
            ServiceCommand::ApproveSession { approval, .. } => Some(approval),
            _ => None,
        })
        .collect();
    assert_eq!(sent.len(), 1);
    assert!(sent[0].capabilities.contains(&Capability::NetworkEndpoints));
    assert_eq!(sent[0].network_endpoints, ["sql.lan"]);

    // Another VM without endpoints: nothing to tick, no capability.
    let mut s = with_request(true);
    s.vms[0].config.network = lightvm_proto::NetworkProfile::Nat;
    s.vms[0].config.endpoints = vec![lightvm_proto::NetworkEndpoint {
        name: "sql.lan".into(),
        host: "127.0.0.1".into(),
        port: 1433,
        virtual_ip: std::net::Ipv4Addr::new(10, 0, 2, 100),
    }];
    let (a2, _hs) = app(s);
    let mut h = harness(a2);
    goto(&mut h, Section::Agents);
    click(&mut h, &format!("Review the request of {CLIENT}"));
    h.get_by_label_contains("This VM is running with NAT networking");
    tap(&mut h, "ubuntu-dev");
    h.get_by_label_contains("This VM has no network endpoints");
}

#[test]
fn request_can_be_denied_from_the_card_and_from_the_form() {
    let (app, hs) = app(with_request(true));
    let mut h = harness(app);
    goto(&mut h, Section::Agents);
    click(&mut h, &format!("Deny the request of {CLIENT}"));
    assert_eq!(actions(&hs), [ServiceCommand::DenySession(3)]);
    hs.clear();
    click(&mut h, &format!("Review the request of {CLIENT}"));
    click(&mut h, &format!("Deny the request of {CLIENT}"));
    assert_eq!(actions(&hs), [ServiceCommand::DenySession(3)]);
    assert_eq!(h.state().reviewing(), None);
    // Closed by the supervisor (denied, expired or withdrawn): gone.
    hs.update(|s| s.agents.requests.clear());
    h.run_steps(4);
    h.get_by_label("No session requests.");
}

#[test]
fn session_can_be_paused_resumed_and_revoked_with_confirmation() {
    let (app, hs) = app(with_session(SessionState::Active, false));
    let mut h = harness(app);
    let banner = "Agent session active on lab \u{b7} claude-desktop \u{b7} keyboard and mouse: you \u{b7} 52 min left";
    h.get_by_label(banner);
    click(&mut h, "Pause the agent session");
    assert_eq!(actions(&hs), [ServiceCommand::PauseSession(session_id(1))]);
    hs.clear();
    hs.update(|s| {
        s.agents.sessions[0].state = SessionState::Paused;
        s.agents.control = Some(AgentControl {
            op: AgentOp::Pause(session_id(1)),
            status: RequestStatus::Done(()),
        });
    });
    h.run_steps(4);
    h.get_by_label_contains("Agent session paused on lab");
    click(&mut h, "Resume the agent session");
    assert_eq!(actions(&hs), [ServiceCommand::ResumeSession(session_id(1))]);
    hs.clear();
    hs.update(|s| {
        s.agents.sessions[0].state = SessionState::Active;
        s.agents.control = Some(AgentControl {
            op: AgentOp::Resume(session_id(1)),
            status: RequestStatus::Done(()),
        });
    });
    h.run_steps(4);

    // Revoke from the Agents page: confirmation first, nothing sent on
    // Cancel.
    goto(&mut h, Section::Agents);
    click(&mut h, &format!("Revoke the session of {CLIENT}"));
    assert!(h.state().dialog_open());
    h.get_by_label("Revoke the session of \u{201c}claude-desktop\u{201d}?");
    h.get_by_label_contains("The VM keeps running.");
    // The dialog blocks the window, but the Emergency Stop stays reachable
    // above it (pointer click, as the user does).
    button(&h, "Emergency Stop: stop all VMs and agent sessions now").click();
    h.run_steps(4);
    assert_eq!(actions(&hs), [ServiceCommand::EmergencyStop]);
    assert!(h.state().dialog_open());
    hs.clear();
    click(&mut h, "Cancel");
    assert!(!h.state().dialog_open());
    assert!(actions(&hs).is_empty());
    click(&mut h, &format!("Revoke the session of {CLIENT}"));
    click(&mut h, "Revoke session");
    assert_eq!(actions(&hs), [ServiceCommand::RevokeSession(session_id(1))]);
    h.get_by_label("Waiting for the supervisor…");
    hs.update(|s| {
        s.agents.control = Some(AgentControl {
            op: AgentOp::Revoke(session_id(1)),
            status: RequestStatus::Done(()),
        });
        s.agents.sessions[0].state = SessionState::Terminated;
        s.agents.sessions[0].termination = Some(TerminationCause::Revoked);
    });
    h.run_steps(4);
    assert!(!h.state().dialog_open());
    // The indicator is gone with the session; it is in the history.
    assert!(h.query_by_label(BANNER_STOP).is_none());
    h.get_by_label("Session history");
    h.get_by_label_contains("revoked by you");
}

#[test]
fn indicator_is_on_every_page_with_the_emergency_stop() {
    let (fake, hs) = FakeSupervisor::new(with_session(SessionState::Active, true));
    let console = LiveConsole::synthetic(
        ConsoleTarget {
            vm_id: vm_id(1),
            vm_name: "lab".into(),
            run_id: common::run_id(1),
        },
        common::deterministic_frame(1280, 800),
    );
    let mut app = LightVmApp::with_live_console(console);
    app.set_supervisor(fake);
    app.set_fixed_time(Some(common::now()));
    let mut h = harness(app);
    let banner = "Agent session active on lab \u{b7} claude-desktop \u{b7} keyboard and mouse: agent \u{b7} 52 min left";
    for s in [
        Section::Dashboard,
        Section::Console,
        Section::Agents,
        Section::Settings,
    ] {
        goto(&mut h, s);
        h.get_by_label(banner);
        assert!(enabled(&h, BANNER_STOP), "{s:?}");
        assert!(enabled(&h, "Pause the agent session"));
        assert!(enabled(&h, "Revoke the agent session"));
        assert!(enabled(&h, "Take keyboard and mouse back from the agent"));
        hs.clear();
        click(&mut h, BANNER_STOP);
        assert_eq!(actions(&hs), [ServiceCommand::EmergencyStop], "{s:?}");
    }
    // Full screen console: side panel hidden, indicator still there.
    goto(&mut h, Section::Console);
    click(&mut h, "Full screen");
    assert!(h.state().fullscreen());
    assert!(
        h.query_by_role_and_label(Role::Button, "Settings")
            .is_none()
    );
    h.get_by_label(banner);
    hs.clear();
    click(&mut h, BANNER_STOP);
    assert_eq!(actions(&hs), [ServiceCommand::EmergencyStop]);
    // Even with the supervisor disconnected the button stays clickable.
    hs.update(|s| s.connected = false);
    h.run_steps(4);
    assert!(enabled(&h, BANNER_STOP));
    // Details from the indicator: the session page.
    click(&mut h, "Open the agent session");
    assert_eq!(h.state().section(), Section::Agents);
    assert_eq!(h.state().agent_session(), Some(session_id(1)));
    assert!(!h.state().fullscreen());
}

fn key_events(hs: &common::Handles) -> usize {
    hs.sent()
        .into_iter()
        .filter_map(|c| match c {
            ServiceCommand::Input(b) => Some(b.events),
            _ => None,
        })
        .flatten()
        .filter(|e| matches!(e, InputEvent::Key(_)))
        .count()
}

fn type_a(h: &mut Harness<'_, LightVmApp>) {
    for pressed in [true, false] {
        h.event(egui::Event::Key {
            key: egui::Key::A,
            physical_key: Some(egui::Key::A),
            pressed,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        });
    }
    h.run_steps(4);
    common::settle(h, |a| a);
}

#[test]
fn console_input_is_not_sent_while_the_agent_has_control() {
    let (fake, hs) = FakeSupervisor::new(with_session(SessionState::Active, false));
    let console = LiveConsole::synthetic(
        ConsoleTarget {
            vm_id: vm_id(1),
            vm_name: "lab".into(),
            run_id: common::run_id(1),
        },
        common::deterministic_frame(1280, 800),
    );
    let mut app = LightVmApp::with_live_console(console);
    app.set_supervisor(fake);
    app.set_fixed_time(Some(common::now()));
    app.set_section(Section::Console);
    let mut h = harness(app);
    // The user types: sent.
    h.get_by_label("Console of lab").click();
    h.run_steps(4);
    type_a(&mut h);
    assert_eq!(key_events(&hs), 2);
    assert!(
        h.query_by_label("Take keyboard and mouse control back")
            .is_none()
    );

    // Hand input to the agent from the indicator.
    hs.clear();
    click(&mut h, "Give keyboard and mouse to the agent");
    assert_eq!(
        actions(&hs),
        [ServiceCommand::SetInputOwner {
            vm_id: vm_id(1),
            owner: InputOwner::Agent {
                session_id: session_id(1)
            },
        }]
    );
    // The supervisor reports the new owner (event).
    hs.update(|s| {
        s.agents.input_owners.insert(
            vm_id(1),
            InputOwner::Agent {
                session_id: session_id(1),
            },
        );
        s.agents.sessions[0].input_owner = true;
    });
    h.run_steps(4);
    h.get_by_label_contains("Input controlled by the agent \u{201c}claude-desktop\u{201d}");
    assert!(h.state().live_console().unwrap().input_blocked());
    hs.clear();
    h.get_by_label("Console of lab").click();
    h.run_steps(4);
    type_a(&mut h);
    let pointer = egui::pos2(500.0, 500.0);
    h.hover_at(pointer);
    h.run_steps(4);
    assert!(
        hs.sent()
            .iter()
            .all(|c| !matches!(c, ServiceCommand::Input(_))),
        "user input sent while the agent has control: {:?}",
        hs.sent()
    );

    // Take control: request, then the owner event restores the input.
    click(&mut h, "Take keyboard and mouse control back");
    assert_eq!(
        actions(&hs),
        [ServiceCommand::SetInputOwner {
            vm_id: vm_id(1),
            owner: InputOwner::User,
        }]
    );
    h.get_by_label("Take keyboard and mouse control back");
    assert!(
        !enabled(&h, "Take keyboard and mouse control back"),
        "taking"
    );
    hs.update(|s| {
        s.agents.control = Some(AgentControl {
            op: AgentOp::InputOwner(vm_id(1), InputOwner::User),
            status: RequestStatus::Done(()),
        });
        s.agents.input_owners.insert(vm_id(1), InputOwner::User);
        s.agents.sessions[0].input_owner = false;
    });
    h.run_steps(4);
    assert!(
        h.query_by_label("Take keyboard and mouse control back")
            .is_none()
    );
    assert!(!h.state().live_console().unwrap().input_blocked());
    hs.clear();
    h.get_by_label("Console of lab").click();
    h.run_steps(4);
    type_a(&mut h);
    assert_eq!(key_events(&hs), 2, "input sent again");
}

#[test]
fn session_page_shows_objective_limits_and_events() {
    let mut s = with_session(SessionState::Active, true);
    s.agents
        .record(at(-8 * 60), Some(session_id(1)), "Session started".into());
    s.agents.record(
        at(-7 * 60),
        Some(session_id(1)),
        "Keyboard and mouse handed to the agent".into(),
    );
    s.agents
        .record(at(-60), None, "Agent access turned on".into());
    let (a2, _hs) = app(s);
    let mut h = harness(a2);
    goto(&mut h, Section::Agents);
    click(&mut h, &format!("Details of the session of {CLIENT}"));
    h.get_by_label("Session \u{b7} claude-desktop");
    h.get_by_label("Agent session on lab");
    h.get_by_label("Check that the text editor saves a file in the home folder");
    h.get_by_label("See the screen (screenshots)");
    h.get_by_label("From 2026-10-11 09:04 UTC to 2026-10-11 10:04 UTC");
    h.get_by_label("09:04:00 UTC");
    h.get_by_label("Session started");
    h.get_by_label("Keyboard and mouse handed to the agent");
    // Events of other sessions or of the switch are not mixed in.
    assert!(h.query_by_label("Agent access turned on").is_none());
    assert_eq!(common::text_fields(&h), 0);
    click(&mut h, "Back to Agents");
    assert_eq!(h.state().agent_session(), None);
}

#[test]
fn agents_page_without_supervisor_explains_why() {
    let mut app = LightVmApp::new(false);
    app.set_section(Section::Agents);
    let h = harness(app);
    h.get_by_label("Agent sessions are not available: the supervisor is not running.");
}

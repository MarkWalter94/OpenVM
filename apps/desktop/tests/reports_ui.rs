//! Session reports in the AI Workspace (F5), whole app with a stand-in
//! supervisor: session history with outcome and source, the session page
//! fed by the report (each outcome, the agent's declaration labelled as a
//! claim, events by page, screenshots only on request), the user's verdict
//! and the automatic check with confirmation, export through a fake save
//! dialog, deletion with confirmation, the live session at the top of the
//! workspace. No process, no IPC.

mod common;

use std::sync::{Arc, Mutex};

use common::{
    FakeSupervisor, connected, report, report_events, session, session_id, shot_image, vm,
};
use egui::accesskit::Role;
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT as _, Queryable as _};
use lightvm_desktop::reports::{USER_VERDICT_RATIONALE, open_report_name};
use lightvm_desktop::service::{
    EventsPage, ExportDone, Failure, ReportControl, ReportOp, ReportOutcome, RequestStatus,
    ServiceCommand, ServiceState,
};
use lightvm_desktop::{LightVmApp, Section};
use lightvm_proto::report::{
    AgentDeclaration, FixtureMarker, OutcomeSource, ReportFormat, SessionVerifier,
};
use lightvm_proto::{
    ErrorCode, SessionState, Sha256Digest, TerminationCause, TestOutcome, VmState,
};

const CLIENT: &str = "claude-desktop";

fn catalog() -> ServiceState {
    let mut s = connected(vec![
        vm(1, "lab", VmState::Running),
        vm(2, "ubuntu-dev", VmState::Stopped),
    ]);
    s.agents.access = Some(true);
    s.agents.sessions_loaded = true;
    s.reports.loaded = true;
    s
}

/// One ended session (revoked, outcome cancelled from the termination)
/// with its header and a page of events.
fn ended() -> ServiceState {
    let mut s = catalog();
    let r = report(
        1,
        1,
        CLIENT,
        Some(TerminationCause::Revoked),
        TestOutcome::Cancelled,
        OutcomeSource::Termination,
    );
    s.reports.list = vec![r.summary()];
    s.reports.headers.insert(r.session_id, r);
    s.reports.events.insert(
        session_id(1),
        EventsPage {
            total: 42,
            offset: 0,
            events: report_events(1),
        },
    );
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
        .with_size(egui::vec2(1100.0, 900.0))
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

/// Commands except the re-reads (lists, headers, event pages).
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

fn open_report(h: &mut Harness<'_, LightVmApp>, client: &str) {
    click(h, Section::Agents.label());
    click(h, &open_report_name(client, common::at(-8 * 60)));
}

fn set_action(hs: &common::Handles, op: ReportOp, status: RequestStatus<ReportOutcome>) {
    hs.update(|s| s.reports.action = Some(ReportControl { op, status }));
}

#[test]
fn history_lists_every_outcome_with_its_source() {
    let mut s = catalog();
    let cases = [
        (
            TestOutcome::Passed,
            OutcomeSource::Verifier,
            "Passed, by the automatic verifier",
        ),
        (
            TestOutcome::Failed,
            OutcomeSource::User,
            "Failed, your verdict",
        ),
        (
            TestOutcome::Inconclusive,
            OutcomeSource::Termination,
            "Inconclusive, from how the session ended",
        ),
        (
            TestOutcome::Cancelled,
            OutcomeSource::Termination,
            "Cancelled, from how the session ended",
        ),
        (
            TestOutcome::TimedOut,
            OutcomeSource::Termination,
            "Timed out, from how the session ended",
        ),
        (
            TestOutcome::InfrastructureError,
            OutcomeSource::Termination,
            "Infrastructure error, from how the session ended",
        ),
    ];
    for (i, (o, src, _)) in cases.iter().enumerate() {
        let n = u8::try_from(i).unwrap() + 1;
        let mut r = report(
            n,
            1,
            &format!("agent-{n}"),
            Some(TerminationCause::Completed),
            *o,
            *src,
        );
        r.started_at = common::at(-3600 * i64::from(n));
        s.reports.list.push(r.summary());
    }
    // A live one, newest first.
    let live = report(
        9,
        1,
        CLIENT,
        None,
        TestOutcome::Inconclusive,
        OutcomeSource::Pending,
    );
    s.reports.list.insert(0, live.summary());
    let (app, hs) = app(s);
    let mut h = harness(app);
    click(&mut h, Section::Agents.label());
    h.get_by_label("AI Workspace");
    h.get_by_label("Session history");
    for (_, _, text) in cases {
        h.get_by_label(&format!("Outcome: {text}"));
    }
    h.get_by_label("Outcome: Inconclusive, pending: session running");
    h.get_by_label("Running");
    // Start and end in UTC, VM, counts.
    h.get_by_label_contains(
        "VM lab \u{b7} 2026-10-11 08:12 UTC \u{2192} 09:10 UTC \u{b7} ended by the agent \u{b7} 42 events \u{b7} 3 screenshots",
    );
    // Newest first: the rows keep the supervisor's order.
    let rows: Vec<String> = h
        .query_all_by_role(Role::Button)
        .filter_map(|n| n.accesskit_node().label())
        .filter(|l| l.starts_with("Open the report of"))
        .collect();
    assert_eq!(rows.len(), 7);
    assert!(rows[0].starts_with(&format!("Open the report of {CLIENT}")));
    assert!(rows[1].starts_with("Open the report of agent-1"));
    assert_eq!(common::text_fields(&h), 0);
    assert!(actions(&hs).is_empty());
}

#[test]
fn session_page_shows_each_outcome_and_where_it_comes_from() {
    let cases = [
        (
            TestOutcome::Passed,
            OutcomeSource::Verifier,
            Some(TerminationCause::Completed),
            "Decided by the automatic verifier \u{201c}fixture_marker v1\u{201d}.",
        ),
        (
            TestOutcome::Failed,
            OutcomeSource::User,
            Some(TerminationCause::Completed),
            "Your verdict, recorded as decided by \u{201c}local user\u{201d} on 2026-10-11 09:11 UTC.",
        ),
        (
            TestOutcome::Inconclusive,
            OutcomeSource::Termination,
            Some(TerminationCause::ClientLost),
            "Default from how the session ended (the agent disconnected; its VM was stopped). Nobody judged it yet.",
        ),
        (
            TestOutcome::Cancelled,
            OutcomeSource::Termination,
            Some(TerminationCause::Revoked),
            "Default from how the session ended (revoked by you). Nobody judged it yet.",
        ),
        (
            TestOutcome::TimedOut,
            OutcomeSource::Termination,
            Some(TerminationCause::GrantExpired),
            "Default from how the session ended (time limit reached). Nobody judged it yet.",
        ),
        (
            TestOutcome::InfrastructureError,
            OutcomeSource::Termination,
            Some(TerminationCause::VmTerminated),
            "Default from how the session ended (the VM stopped). Nobody judged it yet.",
        ),
        (
            TestOutcome::Inconclusive,
            OutcomeSource::Pending,
            None,
            "Pending: the session is still running. Until it ends the outcome reads inconclusive.",
        ),
    ];
    for (outcome, source, termination, text) in cases {
        let mut s = catalog();
        let r = report(1, 1, CLIENT, termination, outcome, source);
        s.reports.list = vec![r.summary()];
        s.reports.headers.insert(r.session_id, r);
        let (app, hs) = app(s);
        let mut h = harness(app);
        open_report(&mut h, CLIENT);
        assert_eq!(h.state().agent_session(), Some(session_id(1)));
        // Opening the page reads the header and the first events.
        let sent = hs.sent();
        assert!(sent.contains(&ServiceCommand::GetSessionReport(session_id(1))));
        assert!(sent.contains(&ServiceCommand::GetSessionReportEvents {
            session_id: session_id(1),
            offset: 0,
            limit: 50,
            tail: false,
        }));
        h.get_by_label("Session \u{b7} claude-desktop");
        h.get_by_label("Agent session on lab");
        h.get_by_label(&format!(
            "Outcome: {}, {}",
            lightvm_desktop::reports::outcome_text(outcome),
            lightvm_desktop::reports::source_short(source)
        ));
        h.get_by_label(text);
        // Limits, policy, versions and evidence of the report.
        h.get_by_label("See the screen (screenshots)");
        h.get_by_label_contains(
            "LightVM service 0.0.0 \u{b7} IPC protocol 1 \u{b7} MCP 2025-11-25",
        );
        h.get_by_label("Clean install");
        h.get_by_label("permission_denied: 1");
        // Live session: the report cannot be deleted.
        assert_eq!(
            enabled(&h, "Delete the report and its screenshots"),
            termination.is_some()
        );
        h.get_by_label("Reading the events\u{2026}");
        assert_eq!(common::text_fields(&h), 0);
    }
}

#[test]
fn agent_declaration_is_labelled_as_a_claim_not_the_outcome() {
    let mut s = ended();
    let id = session_id(1);
    if let Some(r) = s.reports.headers.get_mut(&id) {
        r.outcome = TestOutcome::Inconclusive;
        r.termination = Some(TerminationCause::Completed);
        r.agent_declaration = Some(AgentDeclaration {
            claimed_outcome: TestOutcome::Passed,
            summary: "<b>All done</b>, the file is saved".into(),
            at: common::at(-3 * 60),
        });
    }
    let (app, _hs) = app(s);
    let mut h = harness(app);
    open_report(&mut h, CLIENT);
    h.get_by_label("Agent's declaration");
    h.get_by_label_contains("What the agent claims about its own work. It is not the outcome");
    h.get_by_label("The agent claims: passed");
    // Plain text, never markup.
    h.get_by_label("<b>All done</b>, the file is saved");
    // The outcome stays the supervisor's.
    h.get_by_label("Outcome: Inconclusive, from how the session ended");
    assert!(h.query_by_label_contains("Outcome: Passed").is_none());
    // Its event says the same.
    h.get_by_label("The agent declared passed (its claim, not the outcome)");
}

#[test]
fn events_are_paged_and_screenshots_are_read_only_on_request() {
    let mut s = ended();
    if let Some(p) = s.reports.events.get_mut(&session_id(1)) {
        p.total = 120;
    }
    let (app, hs) = app(s);
    let mut h = harness(app);
    open_report(&mut h, CLIENT);
    h.get_by_label("Events 1\u{2013}6 of 120");
    h.get_by_label("Agent: take a screenshot");
    h.get_by_label("Agent: reset the VM \u{2014} refused (permission_denied)");
    h.get_by_label_contains("Tool vm_keyboard_type: done \u{b7} {\"chars\":12");
    h.get_by_label_contains(
        "Screenshot #1 \u{b7} 1280\u{d7}800 \u{b7} 240 KiB \u{b7} SHA-256 3e3e3e3e3e3e3e3e\u{2026}",
    );
    assert!(!enabled(&h, "Previous events"));
    click(&mut h, "Next events");
    assert!(hs.sent().contains(&ServiceCommand::GetSessionReportEvents {
        session_id: session_id(1),
        offset: 50,
        limit: 50,
        tail: false,
    }));
    // No screenshot read until the user asks for one.
    assert!(actions(&hs).is_empty());
    h.get_by_label_contains("shown here only when you press Show");
    click(&mut h, "Show screenshot 1");
    assert_eq!(
        actions(&hs),
        [ServiceCommand::GetReportScreenshot {
            session_id: session_id(1),
            shot: 0,
        }]
    );
    assert_eq!(h.state().shown_shot(), Some((session_id(1), 0)));
    h.get_by_label("Reading the screenshot\u{2026}");
    hs.update(|s| {
        s.reports
            .set_shot(session_id(1), 0, RequestStatus::Done(shot_image()));
    });
    h.run_steps(3);
    h.get_by_label("Screenshot taken by the agent, 4\u{d7}3");
    h.get_by_label_contains(
        "SHA-256 3e3e3e3e3e3e3e3e3e3e3e3e3e3e3e3e3e3e3e3e3e3e3e3e3e3e3e3e3e3e3e3e (checked)",
    );
    click(&mut h, "Hide the screenshot");
    assert_eq!(h.state().shown_shot(), None);
    assert!(
        h.query_by_label("Screenshot taken by the agent, 4\u{d7}3")
            .is_none()
    );
    // An altered file is not shown.
    hs.clear();
    click(&mut h, "Show screenshot 1");
    // Already read: not read again.
    assert!(actions(&hs).is_empty());
    hs.update(|s| {
        s.reports.set_shot(
            session_id(1),
            0,
            RequestStatus::Failed(Failure::new(
                "the screenshot does not match its SHA-256: not shown",
            )),
        );
    });
    h.run_steps(3);
    h.get_by_label("Screenshot not shown: the screenshot does not match its SHA-256: not shown");
}

#[test]
fn verdict_is_explicit_attributed_and_confirmed() {
    let (app, hs) = app(ended());
    let mut h = harness(app);
    open_report(&mut h, CLIENT);
    click(&mut h, "Set your verdict");
    assert!(h.state().dialog_open());
    h.get_by_label_contains("recorded as decided by \u{201c}local user\u{201d}");
    h.get_by_label_contains("(now cancelled, from how the session ended)");
    // No free text, nothing chosen in advance.
    assert_eq!(common::text_fields(&h), 0);
    assert!(!enabled(&h, "Record verdict"));
    click(&mut h, "Cancel");
    assert!(!h.state().dialog_open());
    assert!(actions(&hs).is_empty());

    click(&mut h, "Set your verdict");
    tap(&mut h, "Failed: the objective was not met");
    assert!(enabled(&h, "Record verdict"));
    click(&mut h, "Record verdict");
    assert_eq!(
        actions(&hs),
        [ServiceCommand::SetSessionVerdict {
            session_id: session_id(1),
            outcome: TestOutcome::Failed,
            rationale: USER_VERDICT_RATIONALE.into(),
        }]
    );
    h.get_by_label("Waiting for the supervisor…");
    // The supervisor records it: the report now carries the user's verdict.
    hs.update(|s| {
        let r = report(
            1,
            1,
            CLIENT,
            Some(TerminationCause::Revoked),
            TestOutcome::Failed,
            OutcomeSource::User,
        );
        s.reports.list = vec![r.summary()];
        s.reports.headers.insert(r.session_id, r);
    });
    set_action(
        &hs,
        ReportOp::Verdict(session_id(1)),
        RequestStatus::Done(ReportOutcome::Updated),
    );
    h.run_steps(4);
    assert!(!h.state().dialog_open());
    h.get_by_label("Your verdict was recorded.");
    h.get_by_label("Outcome: Failed, your verdict");
    h.get_by_label("Verdict history (the last one is the outcome)");
    h.get_by_label_contains("local user: Judged by the local user");
}

#[test]
fn automatic_check_is_confirmed_and_needs_a_screenshot() {
    let (a, hs) = app(ended());
    let mut h = harness(a);
    open_report(&mut h, CLIENT);
    click(&mut h, "Run the automatic check");
    h.get_by_label_contains("LightVM test fixture screen");
    assert!(!enabled(&h, "Run check"));
    tap(&mut h, "The marker should be on screen");
    click(&mut h, "Run check");
    assert_eq!(
        actions(&hs),
        [ServiceCommand::VerifySession {
            session_id: session_id(1),
            verifier: SessionVerifier::FixtureMarker {
                expected: FixtureMarker::Present,
            },
        }]
    );
    set_action(
        &hs,
        ReportOp::Verify(session_id(1)),
        RequestStatus::Failed(Failure::coded(
            ErrorCode::InfrastructureError,
            "infrastructure_error: stored screenshot unreadable",
        )),
    );
    h.run_steps(4);
    // The failure stays in the dialog until it is closed.
    h.get_by_label("infrastructure_error: stored screenshot unreadable");
    click(&mut h, "Cancel");
    h.get_by_label(
        "The automatic check did not run: infrastructure_error: stored screenshot unreadable",
    );

    // Without stored screenshots there is nothing to check.
    let mut s = ended();
    if let Some(r) = s.reports.headers.get_mut(&session_id(1)) {
        r.screenshots_retained = 0;
    }
    let (a, _hs) = app(s);
    let mut h = harness(a);
    open_report(&mut h, CLIENT);
    assert!(!enabled(&h, "Run the automatic check"));
}

#[test]
fn export_goes_through_the_save_dialog_and_says_where_it_was_written() {
    let (mut app, hs) = app(ended());
    let asked: Arc<Mutex<Vec<(String, ReportFormat)>>> = Arc::default();
    let answer: Arc<Mutex<Option<std::path::PathBuf>>> = Arc::default();
    let (a2, b2) = (asked.clone(), answer.clone());
    app.set_report_picker(Some(Box::new(move |name: &str, f: ReportFormat| {
        a2.lock().unwrap().push((name.to_owned(), f));
        b2.lock().unwrap().clone()
    })));
    let mut h = harness(app);
    open_report(&mut h, CLIENT);
    click(&mut h, "Export the report");
    h.get_by_label("Export the session report");
    h.get_by_label_contains("Typed text is never in it, only");
    tap(&mut h, "JSON, the canonical report for tools");
    tap(
        &mut h,
        "Also save the 2 stored screenshots in a folder next to it",
    );
    h.get_by_label(
        "Screenshots can show anything that was on the guest screen, passwords included.",
    );
    // The user cancels the save dialog: nothing is sent, the dialog stays.
    click(&mut h, "Choose where to save\u{2026}");
    assert_eq!(
        asked.lock().unwrap().as_slice(),
        [(
            "lightvm-session-claude-desktop-20261011-0904.json".to_owned(),
            ReportFormat::Json
        )]
    );
    assert!(actions(&hs).is_empty());
    assert!(h.state().dialog_open());

    // Save path in the host's own syntax.
    #[cfg(windows)]
    let shown = r"C:\Users\me\Documents\session.json";
    #[cfg(not(windows))]
    let shown = "/Users/me/Documents/session.json";
    let path = std::path::PathBuf::from(shown);
    *answer.lock().unwrap() = Some(path.clone());
    click(&mut h, "Choose where to save\u{2026}");
    let export = |overwrite| ServiceCommand::ExportSessionReport {
        session_id: session_id(1),
        format: ReportFormat::Json,
        destination: path.clone(),
        overwrite,
        include_screenshots: true,
    };
    assert_eq!(actions(&hs), [export(false)]);
    h.get_by_label("Waiting for the supervisor…");
    // The file exists: the user is asked before it is replaced.
    set_action(
        &hs,
        ReportOp::Export(session_id(1)),
        RequestStatus::Failed(Failure::coded(
            ErrorCode::StateConflict,
            "state_conflict: destination exists",
        )),
    );
    h.run_steps(4);
    h.get_by_label_contains("A file with this name already exists there.");
    hs.clear();
    click(&mut h, "Replace the file");
    assert_eq!(actions(&hs), [export(true)]);
    set_action(
        &hs,
        ReportOp::Export(session_id(1)),
        RequestStatus::Done(ReportOutcome::Exported(ExportDone {
            path: path.clone(),
            format: ReportFormat::Json,
            bytes: 18_432,
            sha256: Sha256Digest::from_bytes([0x77; 32]),
            screenshots: 2,
        })),
    );
    h.run_steps(4);
    h.get_by_label("Report exported");
    let written = format!(
        "Report written to {shown} (18.0 KiB, SHA-256 7777777777777777\u{2026}). 2 screenshots written to the folder session-screenshots next to it."
    );
    let written = written.as_str();
    // In the dialog (and on the page behind it).
    assert_eq!(h.query_all_by_label(written).count(), 2);
    click(&mut h, "Done");
    assert!(!h.state().dialog_open());
    // Also on the page.
    h.get_by_label(written);
    assert_eq!(asked.lock().unwrap().len(), 2);
}

#[test]
fn export_without_a_save_dialog_explains_why() {
    let (app, hs) = app(ended());
    let mut h = harness(app);
    open_report(&mut h, CLIENT);
    click(&mut h, "Export the report");
    click(&mut h, "Choose where to save\u{2026}");
    h.get_by_label("Save dialog not available on this platform.");
    assert!(actions(&hs).is_empty());
}

#[test]
fn report_is_deleted_only_after_confirmation() {
    let (app, hs) = app(ended());
    let mut h = harness(app);
    open_report(&mut h, CLIENT);
    click(&mut h, "Delete the report and its screenshots");
    h.get_by_label("Delete the report?");
    h.get_by_label_contains("its 2 stored screenshots are removed from this computer. Copies you exported are not touched. This cannot be undone.");
    click(&mut h, "Cancel");
    assert!(actions(&hs).is_empty());
    click(&mut h, "Delete the report and its screenshots");
    click(&mut h, "Delete report");
    assert_eq!(
        actions(&hs),
        [ServiceCommand::DeleteSessionReport(session_id(1))]
    );
    hs.update(|s| s.reports.forget(session_id(1)));
    set_action(
        &hs,
        ReportOp::Delete(session_id(1)),
        RequestStatus::Done(ReportOutcome::Deleted),
    );
    h.run_steps(4);
    assert!(!h.state().dialog_open());
    // Back on the workspace, the report gone from the history.
    assert_eq!(h.state().agent_session(), None);
    h.get_by_label("Report deleted, with its stored screenshots.");
    h.get_by_label("No session reports yet.");
}

#[test]
fn workspace_shows_the_live_session_and_its_screen_only_on_request() {
    let mut s = catalog();
    let mut info = session(1, 1, CLIENT, SessionState::Active, true);
    info.capabilities
        .insert(lightvm_proto::Capability::VmScreenshot);
    s.agents.sessions = vec![info];
    s.agents.input_owners.insert(
        common::vm_id(1),
        lightvm_proto::ipc::InputOwner::Agent {
            session_id: session_id(1),
        },
    );
    let r = report(
        1,
        1,
        CLIENT,
        None,
        TestOutcome::Inconclusive,
        OutcomeSource::Pending,
    );
    s.reports.list = vec![r.summary()];
    s.reports.headers.insert(r.session_id, r);
    s.reports.tail.insert(
        session_id(1),
        EventsPage {
            total: 42,
            offset: 37,
            events: report_events(37)[1..].to_vec(),
        },
    );
    let (app, hs) = app(s);
    let mut h = harness(app);
    click(&mut h, Section::Agents.label());
    h.get_by_label("Current session");
    h.get_by_label("Objective stated by the agent");
    h.get_by_label(
        "12 of 10,000 operations \u{b7} 340 of 200,000 input events \u{b7} 4 of 2,000 screenshots",
    );
    h.get_by_label("Agent: reset the VM \u{2014} refused (permission_denied)");
    h.get_by_label("Last 5 of 42 actions recorded by LightVM; all of them are in the report.");
    // The live report is re-read (no event per agent action).
    let sent = hs.sent();
    assert!(sent.contains(&ServiceCommand::GetSessionReport(session_id(1))));
    assert!(sent.contains(&ServiceCommand::GetSessionReportEvents {
        session_id: session_id(1),
        offset: 37,
        limit: 5,
        tail: true,
    }));
    // The screen is not read until asked.
    assert!(actions(&hs).is_empty());
    assert!(
        h.query_by_label_contains("Screenshot taken by the agent")
            .is_none()
    );
    click(&mut h, &format!("Show the latest screenshot of {CLIENT}"));
    // Latest of 3 taken: index 2.
    assert_eq!(
        actions(&hs),
        [ServiceCommand::GetReportScreenshot {
            session_id: session_id(1),
            shot: 2,
        }]
    );
    hs.update(|s| {
        s.reports
            .set_shot(session_id(1), 2, RequestStatus::Done(shot_image()));
    });
    h.run_steps(3);
    h.get_by_label("Screenshot taken by the agent, 4\u{d7}3");
    h.get_by_label_contains("Screenshot 3 \u{b7} 4\u{d7}3");
    click(&mut h, &format!("Hide the screenshots of {CLIENT}"));
    assert!(
        h.query_by_label_contains("Screenshot taken by the agent")
            .is_none()
    );
    // The live session is also in the history, pending.
    h.get_by_label("Outcome: Inconclusive, pending: session running");
}

#[test]
fn report_changed_event_rereads_what_is_shown() {
    let s = ended();
    let follow = lightvm_desktop::service::report_changed(&s, session_id(1));
    assert_eq!(
        follow,
        [
            ServiceCommand::ListSessionReports,
            ServiceCommand::GetSessionReport(session_id(1)),
            ServiceCommand::GetSessionReportEvents {
                session_id: session_id(1),
                offset: 0,
                limit: 50,
                tail: false,
            },
        ]
    );
    // A report the app does not show: only the list.
    assert_eq!(
        lightvm_desktop::service::report_changed(&s, session_id(7)),
        [ServiceCommand::ListSessionReports]
    );
}

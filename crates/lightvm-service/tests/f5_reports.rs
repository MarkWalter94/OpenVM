//! F5 — session reports through the real IPC (real core, in-memory catalog,
//! fake backend): one report per outcome state (`passed`, `failed`,
//! `inconclusive`, `cancelled`, `timed_out`, `infrastructure_error`), the
//! source of each outcome (termination, verifier, user), the agent's
//! declaration as a claim only, the ADR-0019 fixture verifier on the stored
//! screenshot, JSON/Markdown export to a user-chosen file, retention and
//! deletion, survival across a restart
//! (`docs/verification/F5-01-mcp-reports.md`).

use std::path::Path;
use std::time::Duration;

use lightvm_proto::ipc::{
    AgentCall, ClientMessage, Request, ResponseBody, Role, ServerMessage, SessionApproval,
    SessionAuth,
};
use lightvm_proto::report::{
    FixtureMarker, OutcomeSource, ReportEventBody, ReportFormat, SessionReport, SessionVerifier,
};
use lightvm_proto::{
    Capability, CapabilitySet, ErrorCode, OperationId, ReplyError, RunId, SessionId,
    TerminationCause, TestOutcome, VmAction, VmId, VmRequest,
};
use lightvm_service::client::Client;
use lightvm_service::credential::Credential;
use lightvm_service::fakes::{Calls, FakeHost, FakeVmBackend, MemoryAudit, sample_engine};
use lightvm_service::test_support::temp_dir;
use lightvm_service::{ServiceConfig, ServiceHandle, start};

const T: Duration = Duration::from_secs(15);

struct Svc {
    handle: ServiceHandle,
    ui: Credential,
}

/// 1280×800 PNG with the fixture output (ADR-0019) in the given state, or
/// a plain desktop.
fn fixture_png(marker: Option<FixtureMarker>) -> Vec<u8> {
    use lightvm_fixture::verify::{B1, B2, B3, B4, B5_ABSENT, B5_PRESENT};
    let (w, h) = (1280usize, 800usize);
    let mut rgb = vec![48u8; w * h * 3];
    let mut fill = |x0: usize, y0: usize, cw: usize, ch: usize, c: [u8; 3]| {
        for y in y0..y0 + ch * 18 {
            for x in x0..x0 + cw * 9 {
                let o = (y * w + x) * 3;
                rgb[o..o + 3].copy_from_slice(&c);
            }
        }
    };
    if let Some(m) = marker {
        for (i, c) in [B1, B2, B3, B4].into_iter().enumerate() {
            fill(20 + i * 120, 100, 12, 3, c);
        }
        let b5 = match m {
            FixtureMarker::Present => B5_PRESENT,
            FixtureMarker::Absent => B5_ABSENT,
        };
        fill(20, 200, 36, 3, b5);
    }
    let mut out = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut out, w as u32, h as u32);
        enc.set_color(png::ColorType::Rgb);
        enc.set_depth(png::BitDepth::Eight);
        enc.write_header().unwrap().write_image_data(&rgb).unwrap();
    }
    out
}

async fn start_in(dir: &Path, shot: Vec<u8>) -> Svc {
    let calls = Calls::default();
    let backend = FakeVmBackend {
        calls: calls.clone(),
        screenshot_png: shot,
        ..FakeVmBackend::default()
    };
    let engine = sample_engine(backend, MemoryAudit::default());
    let mut cfg = ServiceConfig::new(dir);
    cfg.runtime_dir = Some(dir.join("run"));
    let (handle, ui) = start(cfg, Box::new(engine), Box::new(FakeHost { calls }))
        .await
        .expect("service start");
    Svc { handle, ui }
}

async fn connect(svc: &Svc, cred: &Credential, role: Role) -> Client {
    let mut c = Client::connect(svc.handle.endpoint()).await.unwrap();
    match c.hello(cred).await.unwrap() {
        ServerMessage::HelloOk { role: r, .. } if r == role => c,
        other => panic!("{other:?}"),
    }
}

async fn ok(c: &mut Client, body: Request) -> ResponseBody {
    let name = body.name();
    match tokio::time::timeout(T, c.request(body))
        .await
        .unwrap()
        .unwrap()
    {
        ServerMessage::Response { result, .. } => result,
        other => panic!("{name}: {other:?}"),
    }
}

async fn err(c: &mut Client, body: Request) -> lightvm_proto::PublicError {
    let name = body.name();
    match tokio::time::timeout(T, c.request(body))
        .await
        .unwrap()
        .unwrap()
    {
        ServerMessage::Error {
            error: ReplyError::Operational(e),
            ..
        } => e,
        other => panic!("{name}: expected an error, got {other:?}"),
    }
}

async fn sample_vm(u: &mut Client) -> VmId {
    match ok(u, Request::ListVms).await {
        ResponseBody::VmList { vms } => vms[0].vm_id,
        other => panic!("{other:?}"),
    }
}

struct Agent {
    c: Client,
    auth: SessionAuth,
}

impl Agent {
    async fn call(&mut self, call: AgentCall) -> ServerMessage {
        tokio::time::timeout(
            T,
            self.c.request(Request::Agent {
                session: self.auth.clone(),
                call,
            }),
        )
        .await
        .unwrap()
        .unwrap()
    }

    async fn ok(&mut self, call: AgentCall) -> ResponseBody {
        let name = call.name();
        match self.call(call).await {
            ServerMessage::Response { result, .. } => result,
            other => panic!("{name}: {other:?}"),
        }
    }
}

async fn agent(svc: &Svc, u: &mut Client, vm: VmId, lease_ms: Option<u32>) -> Agent {
    ok(u, Request::SetAgentAccess { enabled: true }).await;
    let cred = match ok(u, Request::CreatePairingCredential).await {
        ResponseBody::PairingCredential { credential, .. } => {
            Credential::from_hex(&credential).unwrap()
        }
        other => panic!("{other:?}"),
    };
    let mut c = connect(svc, &cred, Role::Pairing).await;
    let request_id = match ok(
        &mut c,
        Request::RequestSession {
            client_name: "report-test <b>agent</b>".into(),
            purpose: "Run the fixture\u{1b}[31m and [report](https://example.invalid)".into(),
            requested_vms: vec![vm],
            requested_capabilities: CapabilitySet::new(),
            requested_endpoints: Vec::new(),
        },
    )
    .await
    {
        ResponseBody::SessionRequested { request_id, .. } => request_id,
        other => panic!("{other:?}"),
    };
    c.send(&ClientMessage::Request {
        request_id: 9_000,
        body: Request::AwaitSession { request_id },
    })
    .await
    .unwrap();
    let caps: CapabilitySet = [
        Capability::VmList,
        Capability::VmStatus,
        Capability::VmStart,
        Capability::VmStop,
        Capability::VmScreenshot,
        Capability::VmInput,
        Capability::VmLogs,
    ]
    .into_iter()
    .collect();
    ok(
        u,
        Request::ApproveSession {
            request_id,
            approval: SessionApproval {
                vm_id: vm,
                capabilities: caps,
                duration_s: 3600,
                quotas: None,
                lease_ms,
                give_input: true,
                network_endpoints: Vec::new(),
            },
        },
    )
    .await;
    let auth = match tokio::time::timeout(T, c.recv()).await.unwrap().unwrap() {
        ServerMessage::Response {
            result:
                ResponseBody::SessionGranted {
                    session_id, token, ..
                },
            ..
        } => SessionAuth { session_id, token },
        other => panic!("{other:?}"),
    };
    Agent { c, auth }
}

async fn start_vm(a: &mut Agent, vm: VmId) -> RunId {
    match a
        .ok(AgentCall::Submit {
            request: VmRequest {
                operation_id: OperationId::new_v7(),
                vm_id: vm,
                expected_run_id: None,
                action: VmAction::Start,
            },
        })
        .await
    {
        ResponseBody::Operation {
            operation: Some(op),
            ..
        } => op.run_id.expect("run"),
        other => panic!("{other:?}"),
    }
}

async fn screenshot(a: &mut Agent, vm: VmId, run: RunId) {
    match a
        .ok(AgentCall::Screenshot {
            operation_id: OperationId::new_v7(),
            vm_id: vm,
            expected_run_id: run,
        })
        .await
    {
        ResponseBody::AgentScreenshot { .. } => {}
        other => panic!("{other:?}"),
    }
}

async fn header(u: &mut Client, sid: SessionId) -> SessionReport {
    match ok(u, Request::GetSessionReport { session_id: sid }).await {
        ResponseBody::SessionReport { report } => *report,
        other => panic!("{other:?}"),
    }
}

async fn wait_terminated(u: &mut Client, sid: SessionId) -> SessionReport {
    let t0 = std::time::Instant::now();
    loop {
        let h = header(u, sid).await;
        if h.state == lightvm_proto::SessionState::Terminated {
            return h;
        }
        assert!(t0.elapsed() < T, "session not terminated: {h:?}");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn verify(u: &mut Client, sid: SessionId, expected: FixtureMarker) -> SessionReport {
    match ok(
        u,
        Request::VerifySession {
            session_id: sid,
            verifier: SessionVerifier::FixtureMarker { expected },
        },
    )
    .await
    {
        ResponseBody::SessionReport { report } => *report,
        other => panic!("{other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn passed_failed_by_verifier_and_user_with_declaration_and_export() {
    let dir = temp_dir("f5-rep-verify");
    let svc = start_in(&dir, fixture_png(Some(FixtureMarker::Present))).await;
    let mut u = connect(&svc, &svc.ui, Role::Ui).await;
    let vm = sample_vm(&mut u).await;
    let mut a = agent(&svc, &mut u, vm, None).await;
    let sid = a.auth.session_id;
    let run = start_vm(&mut a, vm).await;
    screenshot(&mut a, vm, run).await;
    // The agent claims success: recorded, never the outcome.
    match a
        .ok(AgentCall::Declare {
            claimed_outcome: TestOutcome::Passed,
            summary: "fixture written <script>alert(1)</script>".into(),
        })
        .await
    {
        ResponseBody::Recorded { seq } => assert!(seq > 0),
        other => panic!("{other:?}"),
    }
    match a.ok(AgentCall::Report { session_id: None }).await {
        ResponseBody::AgentReport { report } => {
            assert_eq!(report.outcome, TestOutcome::Inconclusive);
            assert_eq!(report.outcome_source, OutcomeSource::Pending);
        }
        other => panic!("{other:?}"),
    }
    // Another session's report is not visible to the agent.
    let e = match a
        .call(AgentCall::Report {
            session_id: Some(SessionId::new_v7()),
        })
        .await
    {
        ServerMessage::Error {
            error: ReplyError::Operational(e),
            ..
        } => e,
        other => panic!("{other:?}"),
    };
    assert_eq!(e.code, ErrorCode::NotFound);
    let h = header(&mut u, sid).await;
    assert_eq!(h.outcome_source, OutcomeSource::Pending);
    assert_eq!(h.screenshots_retained, 1);
    assert_eq!(h.run_ids, vec![run]);
    assert!(!h.versions.service.is_empty());
    assert_eq!(h.versions.backend.as_deref(), Some("fake"));
    assert!(h.events.is_empty(), "header without events");
    // End by the agent: default from the termination cause.
    a.ok(AgentCall::EndSession).await;
    let h = wait_terminated(&mut u, sid).await;
    assert_eq!(h.termination, Some(TerminationCause::Completed));
    assert_eq!(h.outcome, TestOutcome::Inconclusive);
    assert_eq!(h.outcome_source, OutcomeSource::Termination);
    // Verifier: marker present expected → passed; absent expected → failed.
    let r = verify(&mut u, sid, FixtureMarker::Present).await;
    assert_eq!(r.outcome, TestOutcome::Passed, "{:?}", r.verdicts);
    assert_eq!(r.outcome_source, OutcomeSource::Verifier);
    assert!(
        r.verdicts
            .last()
            .unwrap()
            .verifier
            .as_ref()
            .unwrap()
            .contains("adr-0019")
    );
    let r = verify(&mut u, sid, FixtureMarker::Absent).await;
    assert_eq!(r.outcome, TestOutcome::Failed);
    // Explicit judgement of the user, attributed.
    let r = match ok(
        &mut u,
        Request::SetSessionVerdict {
            session_id: sid,
            outcome: TestOutcome::Passed,
            rationale: "checked the screen myself".into(),
        },
    )
    .await
    {
        ResponseBody::SessionReport { report } => *report,
        other => panic!("{other:?}"),
    };
    assert_eq!(r.outcome, TestOutcome::Passed);
    assert_eq!(r.outcome_source, OutcomeSource::User);
    assert_eq!(r.verdicts.last().unwrap().decided_by, "local user");
    assert_eq!(r.verdicts.len(), 4, "termination, verifier ×2, user");
    let e = err(
        &mut u,
        Request::SetSessionVerdict {
            session_id: sid,
            outcome: TestOutcome::Passed,
            rationale: "   ".into(),
        },
    )
    .await;
    assert_eq!(e.code, ErrorCode::InvalidArgument);

    // Export: JSON canonical, Markdown escaped, screenshots next to it.
    let out = temp_dir("f5-rep-export");
    let json_path = out.join("report.json");
    let md_path = out.join("report.md");
    match ok(
        &mut u,
        Request::ExportSessionReport {
            session_id: sid,
            format: ReportFormat::Json,
            destination: json_path.to_str().unwrap().into(),
            overwrite: false,
            include_screenshots: true,
        },
    )
    .await
    {
        ResponseBody::SessionReportExported {
            bytes, screenshots, ..
        } => {
            assert_eq!(bytes, std::fs::metadata(&json_path).unwrap().len());
            assert_eq!(screenshots, 1);
        }
        other => panic!("{other:?}"),
    }
    let exported: SessionReport =
        serde_json::from_slice(&std::fs::read(&json_path).unwrap()).unwrap();
    assert_eq!(exported.session_id, sid);
    assert!(exported.report_sha256.is_some());
    assert!(exported.events_total as usize == exported.events.len());
    let shots: Vec<_> = std::fs::read_dir(out.join("report-screenshots"))
        .unwrap()
        .flatten()
        .collect();
    assert_eq!(shots.len(), 1);
    ok(
        &mut u,
        Request::ExportSessionReport {
            session_id: sid,
            format: ReportFormat::Markdown,
            destination: md_path.to_str().unwrap().into(),
            overwrite: false,
            include_screenshots: false,
        },
    )
    .await;
    let md = std::fs::read_to_string(&md_path).unwrap();
    assert!(md.contains("Outcome: **passed** (source: user)"), "{md}");
    assert!(!md.contains('\u{1b}'), "terminal sequence in the Markdown");
    // Untrusted texts only inside code spans or fenced blocks (shown as
    // text: no markup, no link).
    assert!(
        md.contains(
            "```text
Run the fixture[31m and [report](https://example.invalid)
```"
        ),
        "{md}"
    );
    assert!(
        md.contains("| Client | `report-test <b>agent</b>` |"),
        "{md}"
    );
    assert!(
        md.contains(
            "```text
fixture written <script>alert(1)</script>
```"
        ),
        "{md}"
    );
    let e = err(
        &mut u,
        Request::ExportSessionReport {
            session_id: sid,
            format: ReportFormat::Json,
            destination: json_path.to_str().unwrap().into(),
            overwrite: false,
            include_screenshots: false,
        },
    )
    .await;
    assert_eq!(
        e.code,
        ErrorCode::StateConflict,
        "existing file without overwrite"
    );
    for bad in [
        "relative.json",
        "\\\\server\\share\\r.json",
        "C:/no-such-dir-lightvm/x/r.json",
    ] {
        let e = err(
            &mut u,
            Request::ExportSessionReport {
                session_id: sid,
                format: ReportFormat::Json,
                destination: bad.into(),
                overwrite: true,
                include_screenshots: false,
            },
        )
        .await;
        assert_eq!(e.code, ErrorCode::InvalidArgument, "{bad}");
    }
    // Stored screenshot read back by the UI in chunks, hash checked.
    match ok(
        &mut u,
        Request::GetReportScreenshotChunk {
            session_id: sid,
            shot: 0,
            index: 0,
        },
    )
    .await
    {
        ResponseBody::ReportScreenshotChunk { chunks, data, .. } => {
            assert!(chunks >= 1);
            assert!(!data.is_empty());
        }
        other => panic!("{other:?}"),
    }
    // Tampered screenshot: the verifier reports an infrastructure error.
    let shot = dir
        .join("sessions")
        .join(sid.to_string())
        .join("shots")
        .join("0000.png");
    std::fs::write(&shot, b"not the png").unwrap();
    let r = verify(&mut u, sid, FixtureMarker::Present).await;
    assert_eq!(
        r.outcome,
        TestOutcome::InfrastructureError,
        "{:?}",
        r.verdicts
    );
    // Deletion (terminated session): report and screenshots gone.
    match ok(&mut u, Request::DeleteSessionReport { session_id: sid }).await {
        ResponseBody::SessionReportDeleted { session_id } => assert_eq!(session_id, sid),
        other => panic!("{other:?}"),
    }
    let e = err(&mut u, Request::GetSessionReport { session_id: sid }).await;
    assert_eq!(e.code, ErrorCode::NotFound);
    assert!(!dir.join("sessions").join(sid.to_string()).exists());
    svc.handle.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn cancelled_timed_out_and_inconclusive_without_evidence() {
    let dir = temp_dir("f5-rep-causes");
    let svc = start_in(&dir, fixture_png(None)).await;
    let mut u = connect(&svc, &svc.ui, Role::Ui).await;
    let vm = sample_vm(&mut u).await;

    // Revoked by the user → cancelled; a live report cannot be deleted.
    let mut a = agent(&svc, &mut u, vm, None).await;
    let revoked = a.auth.session_id;
    let e = err(
        &mut u,
        Request::DeleteSessionReport {
            session_id: revoked,
        },
    )
    .await;
    assert_eq!(e.code, ErrorCode::StateConflict);
    let run = start_vm(&mut a, vm).await;
    screenshot(&mut a, vm, run).await;
    ok(
        &mut u,
        Request::RevokeSession {
            session_id: revoked,
        },
    )
    .await;
    let h = wait_terminated(&mut u, revoked).await;
    assert_eq!(h.termination, Some(TerminationCause::Revoked));
    assert_eq!(h.outcome, TestOutcome::Cancelled);
    // Fixture not on the screen: the verifier says failed.
    let r = verify(&mut u, revoked, FixtureMarker::Present).await;
    assert_eq!(r.outcome, TestOutcome::Failed);

    // Lease not renewed (1 s) → timed_out.
    let a2 = agent(&svc, &mut u, vm, Some(1000)).await;
    let expired = a2.auth.session_id;
    let h = wait_terminated(&mut u, expired).await;
    assert_eq!(h.termination, Some(TerminationCause::LeaseExpired));
    assert_eq!(h.outcome, TestOutcome::TimedOut);
    // No screenshot: the verifier cannot decide.
    let r = verify(&mut u, expired, FixtureMarker::Present).await;
    assert_eq!(r.outcome, TestOutcome::Inconclusive);
    drop(a2);

    // Emergency Stop → cancelled.
    let mut a3 = agent(&svc, &mut u, vm, None).await;
    let es = a3.auth.session_id;
    let _ = start_vm(&mut a3, vm).await;
    ok(&mut u, Request::EmergencyStop).await;
    let h = wait_terminated(&mut u, es).await;
    assert_eq!(h.termination, Some(TerminationCause::EmergencyStop));
    assert_eq!(h.outcome, TestOutcome::Cancelled);

    // History: newest first, all three.
    match ok(&mut u, Request::ListSessionReports).await {
        ResponseBody::SessionReports { reports } => {
            let ids: Vec<SessionId> = reports.iter().map(|r| r.session_id).collect();
            assert_eq!(ids, vec![es, expired, revoked]);
        }
        other => panic!("{other:?}"),
    }
    // Events by page.
    match ok(
        &mut u,
        Request::GetSessionReportEvents {
            session_id: revoked,
            offset: 0,
            limit: 50,
        },
    )
    .await
    {
        ResponseBody::SessionReportEvents { events, total, .. } => {
            assert_eq!(events.len() as u32, total.min(50));
            assert!(
                events
                    .iter()
                    .any(|e| matches!(&e.body, ReportEventBody::Screenshot { retained: true, .. }))
            );
            assert!(
                events
                    .windows(2)
                    .all(|w| w[0].seq < w[1].seq && w[0].at <= w[1].at)
            );
        }
        other => panic!("{other:?}"),
    }
    let e = err(
        &mut u,
        Request::GetSessionReportEvents {
            session_id: revoked,
            offset: 0,
            limit: 51,
        },
    )
    .await;
    assert_eq!(e.code, ErrorCode::InvalidArgument);
    svc.handle.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn reports_survive_a_restart_and_a_lost_supervisor_is_an_infrastructure_error() {
    let dir = temp_dir("f5-rep-restart");
    let svc = start_in(&dir, fixture_png(None)).await;
    let mut u = connect(&svc, &svc.ui, Role::Ui).await;
    let vm = sample_vm(&mut u).await;
    let mut a = agent(&svc, &mut u, vm, None).await;
    let sid = a.auth.session_id;
    a.ok(AgentCall::ToolNote {
        tool: "vm_keyboard_type".into(),
        ok: true,
        error_code: None,
        detail: serde_json::json!({"chars": 4, "sha256": "0".repeat(64)}),
    })
    .await;
    // Snapshot of the live report as a crashed service would leave it.
    tokio::time::sleep(Duration::from_millis(1500)).await;
    let rdir = dir.join("sessions").join(sid.to_string());
    let live_report = std::fs::read(rdir.join("report.json")).unwrap();
    let live_summary = std::fs::read(rdir.join("summary.json")).unwrap();
    drop(a);
    svc.handle.shutdown().await;
    // Orderly shutdown: the session ended with the UI (`ui_closed`).
    std::fs::write(rdir.join("report.json"), &live_report).unwrap();
    std::fs::write(rdir.join("summary.json"), &live_summary).unwrap();
    let svc = start_in(&dir, fixture_png(None)).await;
    let mut u = connect(&svc, &svc.ui, Role::Ui).await;
    let h = header(&mut u, sid).await;
    assert_eq!(h.termination, Some(TerminationCause::SupervisorLost));
    assert_eq!(h.outcome, TestOutcome::InfrastructureError);
    assert_eq!(h.outcome_source, OutcomeSource::Termination);
    match ok(&mut u, Request::ListSessionReports).await {
        ResponseBody::SessionReports { reports } => {
            assert!(reports.iter().any(|r| r.session_id == sid));
        }
        other => panic!("{other:?}"),
    }
    svc.handle.shutdown().await;
}

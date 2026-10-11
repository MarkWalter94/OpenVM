//! F4 (sessions UI) end-to-end against a real supervisor and a real QEMU
//! (ignored by default). The UI is driven headless with `egui_kittest`; a
//! scripted agent client speaks the real IPC with `lightvm_service::client`:
//!
//! 1. the user turns agent access on and pairs a client from the Agents page
//!    (pairing file written with the user-only ACL);
//! 2. the agent reads the pairing file, connects with the pairing
//!    credential and asks for a session; the UI shows the request with the
//!    verification code the agent received;
//! 3. the user approves in the UI (one VM, capabilities, input handed over,
//!    code confirmed); the agent receives the session and renews its lease;
//! 4. the agent takes screenshots (chunks, SHA-256 checked) and types; the
//!    user's console input is blocked meanwhile;
//! 5. the user takes control back, pauses, resumes and revokes from the
//!    indicator: the agent is refused, then its connection is closed; the
//!    VM keeps running.
//!
//! ```text
//! LIGHTVM_QEMU_DIR=C:/Woarkareas/OpenVM/lab/qemu/qemu-11.1.0 \
//! LIGHTVM_ISO=C:/Woarkareas/OpenVM/lab/media/p5-dati.iso \
//! LIGHTVM_E2E_OUT=<folder for the evidence> \
//! cargo test -p lightvm-desktop --test real_agents -- --ignored --nocapture
//! ```
//!
//! The ISO is attached as installation media only to have a bootable VM
//! that draws a screen (UEFI firmware); no guest is installed.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::{Duration, Instant};

use egui::accesskit::Role;
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT as _, Queryable as _};
use lightvm_desktop::service::{RequestStatus, Service, ServiceCommand, ServiceConfig};
use lightvm_desktop::{LightVmApp, Section};
use lightvm_proto::ipc::{
    AgentCall, ParentLossPolicy, Request, ResponseBody, ServerMessage, SessionAuth, base64_decode,
};
use lightvm_proto::{
    Capability, InputBatch, InputEvent, KeyAction, MediaKind, OperationId, QCode, ReplyError,
    SessionState, Sha256Digest, VmAction, VmId, VmRequest, VmState,
};
use lightvm_service::client::Client;
use lightvm_service::credential::Credential;

fn service_executable() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("LIGHTVM_SERVICE_EXE") {
        return Some(PathBuf::from(p));
    }
    let exe = std::env::current_exe().ok()?;
    let p = exe
        .parent()?
        .parent()?
        .join(format!("lightvm-service{}", std::env::consts::EXE_SUFFIX));
    p.exists().then_some(p)
}

type H = Harness<'static, LightVmApp>;

fn log(t0: Instant, what: &str) {
    println!("E2E [{:7.2} s] {what}", t0.elapsed().as_secs_f64());
}

fn wait(h: &mut H, what: &str, secs: u64, ok: impl Fn(&mut H) -> bool) {
    let t0 = Instant::now();
    while !ok(h) {
        assert!(t0.elapsed() < Duration::from_secs(secs), "timeout: {what}");
        std::thread::sleep(Duration::from_millis(50));
        h.run_steps(2);
    }
}

fn state(h: &H) -> lightvm_desktop::service::ServiceState {
    h.state().service().unwrap().state()
}

fn click(h: &mut H, label: &str) {
    h.get_by_role_and_label(Role::Button, label)
        .click_accesskit();
    h.run_steps(3);
}

fn tap(h: &mut H, label: &str) {
    h.get_by_label(label).click_accesskit();
    h.run_steps(3);
}

/// Commands of the test to the agent.
enum Cmd {
    Screenshot,
    Key(&'static str),
    End,
}

/// What the agent saw (printed as evidence).
#[derive(Debug)]
#[allow(dead_code, reason = "fields read through Debug, in the evidence lines")]
enum Seen {
    Code(String),
    Granted(Vec<Capability>),
    Screenshot {
        bytes: usize,
        chunks: u32,
        sha_ok: bool,
    },
    Ok(&'static str),
    Refused(String),
    Closed(String),
}

fn refusal(m: &ServerMessage) -> String {
    match m {
        ServerMessage::Error {
            error: ReplyError::Operational(e),
            ..
        } => {
            let reason = e
                .details
                .as_ref()
                .and_then(|d| d.get("reason"))
                .map(|r| format!(" reason={r}"))
                .unwrap_or_default();
            format!("{}: {}{reason}", e.code.as_str(), e.message)
        }
        ServerMessage::Closing { reason } => format!("closing: {reason:?}"),
        m => format!("unexpected: {m:?}"),
    }
}

/// The agent: pairing → session request → awaits approval → serves the
/// commands of the test, renewing the lease every 2 s meanwhile.
fn agent(
    endpoint: String,
    credential: Credential,
    vm: VmId,
    cmds: Receiver<Cmd>,
    seen: Sender<Seen>,
    out: PathBuf,
) {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async move {
        let mut c = Client::connect(&endpoint).await.expect("agent connect");
        let hello = c.hello(&credential).await.expect("agent hello");
        assert!(matches!(hello, ServerMessage::HelloOk { .. }), "{hello:?}");
        drop(credential);
        let m = c
            .request(Request::RequestSession {
                client_name: "f4-ui-e2e-agent".into(),
                purpose: "End-to-end check of the sessions UI: screenshots and one key".into(),
                requested_vms: vec![vm],
                requested_capabilities: [
                    Capability::VmList,
                    Capability::VmStatus,
                    Capability::VmScreenshot,
                    Capability::VmInput,
                ]
                .into(),
            })
            .await
            .unwrap();
        let ServerMessage::Response {
            result:
                ResponseBody::SessionRequested {
                    request_id,
                    verification_code,
                    ..
                },
            ..
        } = m
        else {
            panic!("request_session: {m:?}")
        };
        seen.send(Seen::Code(verification_code)).unwrap();
        let m = c
            .request(Request::AwaitSession { request_id })
            .await
            .unwrap();
        let ServerMessage::Response {
            result:
                ResponseBody::SessionGranted {
                    session_id,
                    token,
                    grant,
                    ..
                },
            ..
        } = m
        else {
            panic!("await_session: {m:?}")
        };
        seen.send(Seen::Granted(grant.capabilities.iter().copied().collect()))
            .unwrap();
        let auth = SessionAuth { session_id, token };
        let mut shots = 0u32;
        loop {
            let cmd = match cmds.recv_timeout(Duration::from_secs(2)) {
                Ok(c) => Some(c),
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => None,
                Err(_) => return,
            };
            let call = |call: AgentCall| Request::Agent {
                session: auth.clone(),
                call,
            };
            match cmd {
                None => match c.request(call(AgentCall::RenewLease)).await {
                    Ok(ServerMessage::Response {
                        result: ResponseBody::Lease { .. },
                        ..
                    }) => {}
                    Ok(m) => {
                        let _ = seen.send(Seen::Refused(format!("renew: {}", refusal(&m))));
                    }
                    Err(e) => {
                        let _ = seen.send(Seen::Closed(format!("renew: {e}")));
                        return;
                    }
                },
                Some(Cmd::End) => return,
                Some(Cmd::Screenshot) => {
                    let st = c.request(call(AgentCall::VmStatus { vm_id: vm })).await;
                    let run = match st {
                        Ok(ServerMessage::Response {
                            result: ResponseBody::AgentVm { vm },
                            ..
                        }) => vm.run_id.expect("running"),
                        Ok(m) => {
                            seen.send(Seen::Refused(refusal(&m))).unwrap();
                            continue;
                        }
                        Err(e) => {
                            seen.send(Seen::Closed(e.to_string())).unwrap();
                            return;
                        }
                    };
                    let op = OperationId::new_v7();
                    let m = match c
                        .request(call(AgentCall::Screenshot {
                            operation_id: op,
                            vm_id: vm,
                            expected_run_id: run,
                        }))
                        .await
                    {
                        Ok(m) => m,
                        Err(e) => {
                            seen.send(Seen::Closed(e.to_string())).unwrap();
                            return;
                        }
                    };
                    let ServerMessage::Response {
                        result:
                            ResponseBody::AgentScreenshot {
                                screenshot, chunks, ..
                            },
                        ..
                    } = m
                    else {
                        seen.send(Seen::Refused(refusal(&m))).unwrap();
                        continue;
                    };
                    let mut png = Vec::new();
                    for index in 0..chunks {
                        let m = c
                            .request(call(AgentCall::ScreenshotChunk {
                                operation_id: op,
                                index,
                            }))
                            .await
                            .unwrap();
                        let ServerMessage::Response {
                            result: ResponseBody::ScreenshotChunk { data, .. },
                            ..
                        } = m
                        else {
                            panic!("chunk {index}: {m:?}")
                        };
                        png.extend(base64_decode(&data).unwrap());
                    }
                    let sha_ok = {
                        use sha2::Digest as _;
                        Sha256Digest::from_bytes(sha2::Sha256::digest(&png).into())
                            == screenshot.sha256
                    };
                    shots += 1;
                    let _ = std::fs::write(out.join(format!("agent-screenshot-{shots}.png")), &png);
                    seen.send(Seen::Screenshot {
                        bytes: png.len(),
                        chunks,
                        sha_ok,
                    })
                    .unwrap();
                }
                Some(Cmd::Key(q)) => {
                    let st = c.request(call(AgentCall::VmStatus { vm_id: vm })).await;
                    let (run, geometry) = match st {
                        Ok(ServerMessage::Response {
                            result: ResponseBody::AgentVm { vm },
                            ..
                        }) => (vm.run_id.unwrap(), vm.geometry.unwrap()),
                        Ok(m) => {
                            seen.send(Seen::Refused(refusal(&m))).unwrap();
                            continue;
                        }
                        Err(e) => {
                            seen.send(Seen::Closed(e.to_string())).unwrap();
                            return;
                        }
                    };
                    let key = |down| {
                        InputEvent::Key(KeyAction {
                            qcode: QCode::parse(q).unwrap(),
                            down,
                        })
                    };
                    let req = VmRequest {
                        operation_id: OperationId::new_v7(),
                        vm_id: vm,
                        expected_run_id: Some(run),
                        action: VmAction::Input(InputBatch {
                            vm_id: vm,
                            run_id: run,
                            geometry,
                            events: vec![key(true), key(false)],
                        }),
                    };
                    match c.request(call(AgentCall::Submit { request: req })).await {
                        Ok(ServerMessage::Response {
                            result: ResponseBody::Operation { operation, .. },
                            ..
                        }) => {
                            let err = operation.and_then(|o| o.error);
                            match err {
                                None => seen.send(Seen::Ok("input")).unwrap(),
                                Some(code) => seen
                                    .send(Seen::Refused(format!("operation {}", code.as_str())))
                                    .unwrap(),
                            }
                        }
                        Ok(m) => seen.send(Seen::Refused(refusal(&m))).unwrap(),
                        Err(e) => {
                            seen.send(Seen::Closed(e.to_string())).unwrap();
                            return;
                        }
                    }
                }
            }
        }
    });
}

fn next(seen: &Receiver<Seen>, h: &mut H) -> Seen {
    let t0 = Instant::now();
    loop {
        match seen.try_recv() {
            Ok(s) => return s,
            Err(std::sync::mpsc::TryRecvError::Empty) => {
                assert!(t0.elapsed() < Duration::from_secs(60), "agent silent");
                std::thread::sleep(Duration::from_millis(30));
                h.run_steps(1);
            }
            Err(e) => panic!("agent gone: {e}"),
        }
    }
}

#[test]
#[ignore = "requires a real QEMU and an ISO: set LIGHTVM_QEMU_DIR and LIGHTVM_ISO"]
fn sessions_ui_with_a_real_agent_and_qemu() {
    let (Some(qemu), Some(iso)) = (
        std::env::var_os("LIGHTVM_QEMU_DIR").map(PathBuf::from),
        std::env::var_os("LIGHTVM_ISO").map(PathBuf::from),
    ) else {
        eprintln!("LIGHTVM_QEMU_DIR or LIGHTVM_ISO not set: test skipped");
        return;
    };
    let t0 = Instant::now();
    let exe = service_executable().expect("lightvm-service built");
    let dir = std::env::temp_dir().join(format!("lightvm-f4ui-e2e-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let out = std::env::var_os("LIGHTVM_E2E_OUT")
        .map(PathBuf::from)
        .unwrap_or_else(|| dir.join("evidence"));
    std::fs::create_dir_all(&out).unwrap();
    let cfg = ServiceConfig {
        executable: exe,
        catalog: dir.join("catalog"),
        qemu_dir: Some(qemu),
        runtime_dir: Some(dir.join("run")),
        audit_log: Some(dir.join("audit.jsonl")),
        policy: ParentLossPolicy::KillVms,
        pairing_dir: Some(dir.join("pairing")),
    };
    let ctx_holder: Arc<std::sync::Mutex<Option<egui::Context>>> = Arc::default();
    let c2 = ctx_holder.clone();
    let service = Service::start(
        &cfg,
        Arc::new(move || {
            if let Some(c) = c2.lock().unwrap().as_ref() {
                c.request_repaint();
            }
        }),
    )
    .expect("supervisor start");
    let mut app = LightVmApp::new(false);
    app.set_service(service, &cfg);
    let mut h: H = Harness::builder()
        .with_size(egui::vec2(1280.0, 1000.0))
        .with_max_steps(10_000_000)
        .build_ui_state(|ui, app: &mut LightVmApp| app.ui(ui), app);
    *ctx_holder.lock().unwrap() = Some(h.ctx.clone());
    h.run_steps(3);
    wait(&mut h, "connected", 30, |h| state(h).connected);
    log(t0, "supervisor connected");
    wait(&mut h, "access read", 10, |h| {
        state(h).agents.access.is_some()
    });
    assert_eq!(state(&h).agents.access, Some(false), "off at start");
    log(t0, "agent access read: off");

    // A small VM booting the ISO (firmware screen).
    let send = |h: &H, c: ServiceCommand| assert!(h.state().service().unwrap().send(c));
    send(
        &h,
        ServiceCommand::ImportMedia {
            path: iso,
            kind: MediaKind::InstallIso,
            origin: "f4ui e2e".into(),
        },
    );
    wait(&mut h, "ISO imported", 600, |h| {
        !state(h).import.is_pending()
    });
    let RequestStatus::Done(media) = state(&h).import else {
        panic!("import: {:?}", state(&h).import)
    };
    let mut config = lightvm_desktop::vm_settings::default_config("f4ui-e2e").unwrap();
    config.vcpus = 1;
    config.memory_mib = 1024;
    config.disk_gib = 4;
    config.install_media = Some(media.id);
    send(&h, ServiceCommand::Create(Box::new(config)));
    wait(&mut h, "VM created", 120, |h| !state(h).create.is_pending());
    let RequestStatus::Done(vm) = state(&h).create else {
        panic!("create: {:?}", state(&h).create)
    };
    send(&h, ServiceCommand::Start(vm));
    wait(&mut h, "VM running", 120, |h| {
        state(h).vm(vm).is_some_and(|v| v.state == VmState::Running)
    });
    log(t0, "VM running");

    // 1. Agent access on, pairing file through the dialog.
    click(&mut h, "Agents");
    tap(&mut h, "Allow agents to request sessions");
    wait(&mut h, "access on", 10, |h| {
        state(h).agents.access == Some(true)
    });
    log(t0, "agent access turned on in the UI");
    click(&mut h, "Pair a client");
    click(&mut h, "Write pairing file");
    wait(&mut h, "pairing file", 20, |h| {
        !matches!(state(h).agents.pairing, RequestStatus::Pending)
    });
    let RequestStatus::Done(pairing) = state(&h).agents.pairing else {
        panic!("pairing: {:?}", state(&h).agents.pairing)
    };
    h.get_by_label("Pairing file ready");
    click(&mut h, "Done");
    let file: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&pairing.path).unwrap()).unwrap();
    let endpoint = file["endpoint"].as_str().unwrap().to_owned();
    let credential = Credential::from_hex(file["credential"].as_str().unwrap()).unwrap();
    assert!(
        !format!("{:?}", state(&h)).contains(file["credential"].as_str().unwrap()),
        "credential in the UI state"
    );
    #[cfg(windows)]
    {
        let acl = std::process::Command::new(r"C:\Windows\System32\icacls.exe")
            .arg(pairing.path.parent().unwrap())
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&acl.stdout);
        let entries = text.lines().filter(|l| l.contains(":(")).count();
        println!("E2E pairing folder ACL entries={entries}");
        assert_eq!(entries, 1);
    }
    log(t0, "pairing file written and read by the agent");

    // 2. The agent asks for a session; the UI shows its code.
    let (cmd_tx, cmd_rx) = channel();
    let (seen_tx, seen_rx) = channel();
    let out2 = out.clone();
    let agent_thread =
        std::thread::spawn(move || agent(endpoint, credential, vm, cmd_rx, seen_tx, out2));
    let Seen::Code(code) = next(&seen_rx, &mut h) else {
        panic!("no code")
    };
    wait(&mut h, "request in the UI", 10, |h| {
        !state(h).agents.requests.is_empty()
    });
    h.get_by_label(&format!("Verification code {code}"));
    log(
        t0,
        &format!("request shown in the UI with the agent's code {code}"),
    );

    // 3. Approval in the UI.
    click(&mut h, "Review the request of f4-ui-e2e-agent");
    assert!(
        h.get_by_label("f4ui-e2e").accesskit_node().toggled()
            == Some(egui::accesskit::Toggled::True),
        "requested VM preselected"
    );
    tap(&mut h, "Give keyboard and mouse to the agent now");
    tap(&mut h, &format!("The agent shows the code {code}"));
    let approved = Instant::now();
    click(&mut h, "Approve the session of f4-ui-e2e-agent");
    let Seen::Granted(caps) = next(&seen_rx, &mut h) else {
        panic!("not granted")
    };
    println!(
        "E2E approval → agent granted in {:.1} ms, capabilities {caps:?}",
        approved.elapsed().as_secs_f64() * 1000.0
    );
    wait(&mut h, "session live in the UI", 10, |h| {
        state(h).agents.live().next().is_some() && state(h).agents.agent_has_input(vm)
    });
    let banner = h
        .query_all_by_label_contains("Agent session active on f4ui-e2e")
        .count();
    assert_eq!(banner, 1, "indicator");
    log(
        t0,
        "session approved; indicator shown, input with the agent",
    );

    // 4. The agent looks and types; the user's console is blocked.
    let _ = cmd_tx.send(Cmd::Screenshot);
    let s = next(&seen_rx, &mut h);
    println!("E2E agent screenshot: {s:?}");
    assert!(matches!(s, Seen::Screenshot { sha_ok: true, .. }));
    let _ = cmd_tx.send(Cmd::Key("ret"));
    let s = next(&seen_rx, &mut h);
    println!("E2E agent input while owning the input: {s:?}");
    assert!(matches!(s, Seen::Ok("input")));
    click(&mut h, "Console");
    click(&mut h, "Open the console of f4ui-e2e");
    wait(&mut h, "console open", 60, |h| {
        h.state().live_console().is_some_and(|c| c.geometry().0 > 0)
    });
    wait(&mut h, "user input blocked", 10, |h| {
        h.state()
            .live_console()
            .is_some_and(lightvm_desktop::console::LiveConsole::input_blocked)
    });
    h.get_by_label_contains("Input controlled by the agent");
    log(
        t0,
        "console shows the agent has control; user input blocked",
    );

    // 5a. Take control back: the agent is refused.
    click(&mut h, "Take keyboard and mouse back from the agent");
    wait(&mut h, "input back to the user", 10, |h| {
        !state(h).agents.agent_has_input(vm)
    });
    let _ = cmd_tx.send(Cmd::Key("ret"));
    let s = next(&seen_rx, &mut h);
    println!("E2E agent input after take control: {s:?}");
    assert!(matches!(&s, Seen::Refused(r) if r.contains("permission_denied")));
    log(t0, "control taken back; agent input refused");

    // 5b. Pause: actions refused, observation allowed; resume.
    click(&mut h, "Pause the agent session");
    wait(&mut h, "paused", 10, |h| {
        state(h)
            .agents
            .live()
            .any(|s| s.state == SessionState::Paused)
    });
    let _ = cmd_tx.send(Cmd::Key("ret"));
    let s = next(&seen_rx, &mut h);
    println!("E2E agent input while paused: {s:?}");
    assert!(matches!(&s, Seen::Refused(r) if r.contains("permission_denied")));
    let _ = cmd_tx.send(Cmd::Screenshot);
    let s = next(&seen_rx, &mut h);
    println!("E2E agent screenshot while paused: {s:?}");
    assert!(matches!(s, Seen::Screenshot { sha_ok: true, .. }));
    click(&mut h, "Resume the agent session");
    wait(&mut h, "resumed", 10, |h| {
        state(h)
            .agents
            .live()
            .any(|s| s.state == SessionState::Active)
    });
    log(t0, "paused and resumed from the indicator");

    // 5c. Revoke with confirmation: connection closed, VM still running.
    click(&mut h, "Revoke the agent session");
    let revoked = Instant::now();
    click(&mut h, "Revoke session");
    wait(&mut h, "revoked", 10, |h| {
        state(h).agents.live().next().is_none()
    });
    println!(
        "E2E revoke → session terminated in the UI in {:.1} ms",
        revoked.elapsed().as_secs_f64() * 1000.0
    );
    let _ = cmd_tx.send(Cmd::Key("ret"));
    let s = next(&seen_rx, &mut h);
    println!("E2E agent after revocation: {s:?}");
    assert!(matches!(s, Seen::Closed(_) | Seen::Refused(_)));
    assert_eq!(
        state(&h).vm(vm).map(|v| v.state),
        Some(VmState::Running),
        "the VM keeps running"
    );
    assert!(
        h.query_by_label("Emergency Stop: stop all VMs and agent sessions")
            .is_none(),
        "indicator gone"
    );
    click(&mut h, "Agents");
    h.get_by_label_contains("revoked by you");
    // The session page lists what the app saw, request included.
    // F5: the ended session is in the history through its report.
    let open = h
        .query_all_by_role(Role::Button)
        .filter_map(|n| n.accesskit_node().label())
        .find(|l| l.starts_with("Open the report of f4-ui-e2e-agent"))
        .unwrap_or_else(|| "Details of the session of f4-ui-e2e-agent".into());
    click(&mut h, &open);
    for t in [
        "Session started",
        "Keyboard and mouse handed to the agent",
        "Keyboard and mouse back to you",
        "Session paused",
        "Session resumed",
        "Session ended: revoked by you",
    ] {
        h.get_by_label(t);
    }
    h.get_by_label_contains("Session request from \u{201c}f4-ui-e2e-agent\u{201d}, code");
    let log_lines: Vec<String> = state(&h)
        .agents
        .log
        .iter()
        .map(|e| format!("{} {}", e.at, e.text))
        .collect();
    println!(
        "E2E session events seen by the app:\n  {}",
        log_lines.join("\n  ")
    );
    log(t0, "revoked; VM still running");
    let _ = cmd_tx.send(Cmd::End);
    let _ = agent_thread.join();

    let mut app = h.into_state();
    let outcome = app.stop_service();
    println!("E2E service_stop={outcome:?}");
    assert!(!pairing.path.exists(), "pairing file removed at stop");
    let audit = std::fs::read_to_string(dir.join("audit.jsonl")).unwrap_or_default();
    let actions = [
        "agent_access_enable",
        "session_request",
        "session_create",
        "input_control",
        "screenshot_read",
        "session_pause",
        "session_resume",
        "session_terminate",
    ];
    for a in actions {
        println!(
            "E2E audit {a}: {}",
            audit.matches(&format!("\"{a}\"")).count()
        );
    }
    let _ = std::fs::write(out.join("audit.jsonl"), &audit);
    if std::env::var_os("LIGHTVM_E2E_KEEP").is_none() {
        let _ = std::fs::remove_dir_all(&dir);
    }
    let _ = Section::Agents;
}

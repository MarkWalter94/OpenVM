//! Console file controls through the production worker and RPC dispatch, with
//! a stand-in supervisor. Files are real; the guest replies are simulated.
mod common;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use common::{FakeSupervisor, connected, deterministic_frame, run_id, vm, vm_id};
use egui::accesskit::Role;
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT as _, Queryable as _};
use lightvm_desktop::console::{ConsoleTarget, LiveConsole};
use lightvm_desktop::service::{RequestStatus, ServiceCommand};
use lightvm_desktop::{LightVmApp, Section};
use lightvm_proto::{
    RunId, ToolCall, ToolFileEntry, ToolFileReply, ToolFileRequest, ToolReply, VmState,
};

struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        let p = std::env::temp_dir().join(format!("lightvm-transfer-ui-{}", RunId::new_v7()));
        std::fs::create_dir(&p).unwrap();
        Self(p)
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn harness(
    enabled: bool,
    source: PathBuf,
    destination: PathBuf,
) -> (Harness<'static, LightVmApp>, common::Handles) {
    let mut v = vm(1, "tools-test", VmState::Running);
    v.config.tools.enabled = enabled;
    v.config.tools.files_transfer = enabled;
    v.config.tools.clipboard = enabled;
    let (fake, hs) = FakeSupervisor::new(connected(vec![v]));
    let console = LiveConsole::synthetic(
        ConsoleTarget {
            vm_id: vm_id(1),
            vm_name: "tools-test".into(),
            run_id: run_id(1),
        },
        deterministic_frame(1280, 800),
    );
    let mut app = LightVmApp::with_live_console(console);
    app.set_supervisor(fake);
    app.set_section(Section::Console);
    app.set_transfer_pickers(
        Some(Box::new(move |_| Some(source.clone()))),
        Some(Box::new(move |_| Some(destination.clone()))),
    );
    let mut h = Harness::builder()
        .with_size(egui::vec2(1200.0, 1000.0))
        .build_ui_state(|ui, app: &mut LightVmApp| app.ui(ui), app);
    h.run_steps(4);
    (h, hs)
}

fn respond(hs: &common::Handles, request: &ToolFileRequest, reply: ToolFileReply) {
    request.validate_guest().unwrap();
    hs.update(|s| {
        s.tools = Some((
            vm_id(1),
            run_id(1),
            RequestStatus::Done(ToolReply::File { reply }),
        ))
    });
}

fn pump(
    h: &mut Harness<'_, LightVmApp>,
    hs: &common::Handles,
    done: impl Fn(&Harness<'_, LightVmApp>) -> bool,
    mut guest: impl FnMut(&ToolFileRequest) -> ToolFileReply,
) {
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut consumed = 0;
    loop {
        h.run_steps(1);
        let sent = hs.sent();
        for command in &sent[consumed..] {
            if let ServiceCommand::GuestTools {
                call: ToolCall::File { request },
                ..
            } = command
            {
                respond(hs, request, guest(request));
            }
        }
        consumed = sent.len();
        if done(h) {
            break;
        }
        assert!(Instant::now() < deadline, "transfer did not finish");
        std::thread::sleep(Duration::from_millis(2));
    }
}

#[test]
fn disabled_tools_expose_no_transfer_controls() {
    let (h, _) = harness(false, PathBuf::new(), PathBuf::new());
    assert!(
        h.query_by_role_and_label(Role::Button, "Send file…")
            .is_none()
    );
    assert!(
        h.query_by_role_and_label(Role::Button, "Refresh Outbox")
            .is_none()
    );
}

#[test]
fn send_file_uses_inbox_streaming_and_survives_navigation() {
    let root = Temp::new();
    let source = root.0.join("build.bin");
    std::fs::write(&source, vec![0x42; 40000]).unwrap();
    let (mut h, hs) = harness(true, source, root.0.join("result.bin"));
    h.get_by_role_and_label(Role::Button, "Send file…").click();
    h.run_steps(2);
    assert!(
        h.get_by_role_and_label(Role::Button, "Host text → Guest clipboard")
            .accesskit_node()
            .is_disabled()
    );
    h.state_mut().set_section(Section::Dashboard);
    let mut bytes = 0;
    let mut committed = false;
    pump(
        &mut h,
        &hs,
        |h| !h.state().file_transfer_active(),
        |request| match request {
            ToolFileRequest::Begin {
                share, path, size, ..
            } => {
                assert_eq!(share, "inbox");
                assert_eq!(path.components(), &["build.bin"]);
                assert_eq!(*size, 40000);
                ToolFileReply::Transfer {
                    transfer: "ab".repeat(32),
                }
            }
            ToolFileRequest::Write { offset, data, .. } => {
                assert_eq!(*offset, bytes);
                bytes += (data.len() / 2) as u64;
                ToolFileReply::Ok
            }
            ToolFileRequest::Commit { .. } => {
                committed = true;
                ToolFileReply::Ok
            }
            _ => panic!("unexpected request"),
        },
    );
    assert!(committed);
    assert_eq!(bytes, 40000);
}

#[test]
fn outbox_receive_uses_the_selected_host_destination() {
    let root = Temp::new();
    let destination = root.0.join("chosen.bin");
    let (mut h, hs) = harness(true, PathBuf::new(), destination.clone());
    h.get_by_role_and_label(Role::Button, "Refresh Outbox")
        .click();
    pump(
        &mut h,
        &hs,
        |h| {
            h.query_by_role_and_label(Role::Button, "Receive…")
                .is_some()
        },
        |request| {
            assert!(matches!(request, ToolFileRequest::List { share, .. } if share == "outbox"));
            ToolFileReply::Entries {
                entries: vec![ToolFileEntry {
                    name: "guest.bin".into(),
                    size: 3,
                    directory: false,
                }],
            }
        },
    );
    hs.clear();
    h.get_by_role_and_label(Role::Button, "Receive…").click();
    pump(
        &mut h,
        &hs,
        |h| h.query_by_label("File transfer completed").is_some(),
        |request| {
            assert!(
                matches!(request, ToolFileRequest::Read { share, path, offset: 0, .. } if share == "outbox" && path.components() == ["guest.bin"])
            );
            ToolFileReply::Data {
                offset: 0,
                data: "010203".into(),
                eof: true,
            }
        },
    );
    assert_eq!(std::fs::read(destination).unwrap(), [1, 2, 3]);
}

#[cfg(any(windows, target_os = "macos"))]
#[test]
fn prepared_drag_contains_only_the_selected_outbox_file_and_cleans_its_cache() {
    let (mut h, hs) = harness(true, PathBuf::new(), PathBuf::new());
    h.get_by_role_and_label(Role::Button, "Refresh Outbox")
        .click();
    pump(
        &mut h,
        &hs,
        |h| {
            h.query_by_role_and_label(Role::Button, "Prepare drag…")
                .is_some()
        },
        |_| ToolFileReply::Entries {
            entries: vec![ToolFileEntry {
                name: "guest.log".into(),
                directory: false,
                size: 3,
            }],
        },
    );
    hs.clear();
    h.get_by_role_and_label(Role::Button, "Prepare drag…")
        .click();
    pump(
        &mut h,
        &hs,
        |h| {
            h.query_by_label("Drag guest.log to a host folder")
                .is_some()
        },
        |request| {
            assert!(
                matches!(request, ToolFileRequest::Read { share, path, .. } if share == "outbox" && path.components() == ["guest.log"])
            );
            ToolFileReply::Data {
                offset: 0,
                data: "010203".into(),
                eof: true,
            }
        },
    );
    let at = h
        .get_by_label("Drag guest.log to a host folder")
        .rect()
        .center();
    h.hover_at(at);
    h.run_steps(1);
    h.drag_at(at);
    h.run_steps(1);
    h.hover_at(at + egui::vec2(30.0, 10.0));
    h.run_steps(2);
    let path = h
        .state_mut()
        .take_file_drag()
        .expect("only the prepared file can start a native drag");
    assert_eq!(path.file_name().unwrap(), "guest.log");
    assert_eq!(std::fs::read(&path).unwrap(), [1, 2, 3]);
    h.drop_at(at);
    h.run_steps(1);
    h.get_by_role_and_label(Role::Button, "Clear prepared copy")
        .click();
    h.run_steps(2);
    assert!(!path.exists());
    assert!(!path.parent().unwrap().exists());
}

#[derive(Debug)]
struct DropPath(PathBuf);
impl egui::DroppedFile for DropPath {
    fn path(&self) -> &Path {
        &self.0
    }
    fn bytes(&self) -> Result<Vec<u8>, String> {
        panic!("drop must stream from path, never load all bytes")
    }
}

#[test]
fn dropping_on_console_streams_a_local_file() {
    let root = Temp::new();
    let source = root.0.join("dropped.bin");
    std::fs::write(&source, [1, 2, 3]).unwrap();
    let (mut h, hs) = harness(true, PathBuf::new(), PathBuf::new());
    let rect = h.state().live_console().unwrap().image_rect().unwrap();
    h.hover_at(rect.center());
    h.input_mut().dropped_files.push(Arc::new(DropPath(source)));
    let mut committed = false;
    pump(
        &mut h,
        &hs,
        |h| h.query_by_label("File transfer completed").is_some(),
        |request| match request {
            ToolFileRequest::Begin { path, .. } => {
                assert_eq!(path.components(), &["dropped.bin"]);
                ToolFileReply::Transfer {
                    transfer: "ab".repeat(32),
                }
            }
            ToolFileRequest::Write { .. } => ToolFileReply::Ok,
            ToolFileRequest::Commit { .. } => {
                committed = true;
                ToolFileReply::Ok
            }
            _ => panic!("unexpected request"),
        },
    );
    assert!(committed);
}

#[test]
fn cancel_button_cancels_guest_staging_and_keeps_clipboard_separate() {
    let root = Temp::new();
    let source = root.0.join("build.bin");
    std::fs::write(&source, vec![1; 40000]).unwrap();
    let (mut h, hs) = harness(true, source, PathBuf::new());
    h.get_by_role_and_label(Role::Button, "Send file…").click();
    // Hold Begin in flight until the user has cancelled it. Its reply still
    // supplies the transfer id needed to clean up the guest staging file.
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        h.run_steps(1);
        if hs.sent().iter().any(|c| {
            matches!(
                c,
                ServiceCommand::GuestTools {
                    call: ToolCall::File {
                        request: ToolFileRequest::Begin { .. }
                    },
                    ..
                }
            )
        }) {
            break;
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(2));
    }
    h.get_by_role_and_label(Role::Button, "Cancel transfer")
        .click();
    h.run_steps(2);
    hs.update(|s| {
        s.tools = Some((
            vm_id(1),
            run_id(1),
            RequestStatus::Done(ToolReply::File {
                reply: ToolFileReply::Transfer {
                    transfer: "ab".repeat(32),
                },
            }),
        ))
    });
    hs.clear();
    let mut cancelled = false;
    pump(
        &mut h,
        &hs,
        |h| !h.state().file_transfer_active(),
        |request| {
            assert!(matches!(request, ToolFileRequest::Cancel { .. }));
            cancelled = true;
            ToolFileReply::Ok
        },
    );
    assert!(cancelled);
    h.get_by_label_contains("Transfer cancelled");
    assert!(
        !h.get_by_role_and_label(Role::Button, "Send file…")
            .accesskit_node()
            .is_disabled()
    );
}

#[test]
fn console_files_visual_snapshot() {
    let (mut h, _) = harness(true, PathBuf::new(), PathBuf::new());
    h.snapshot("console_files");
}

#[test]
fn changed_run_cancels_without_sending_file_data_to_the_next_run() {
    let root = Temp::new();
    let source = root.0.join("build.bin");
    std::fs::write(&source, vec![1; 40000]).unwrap();
    let (mut h, hs) = harness(true, source, PathBuf::new());
    h.get_by_role_and_label(Role::Button, "Send file…").click();
    h.run_steps(1);
    hs.update(|s| {
        s.vms[0].run_id = Some(run_id(2));
        s.tools = None;
    });
    // A request already dispatched before the observed change may be refused
    // by the supervisor; all requests retain the original run identity.
    pump(
        &mut h,
        &hs,
        |h| !h.state().file_transfer_active(),
        |request| {
            assert!(!matches!(
                request,
                ToolFileRequest::Write { .. } | ToolFileRequest::Commit { .. }
            ));
            ToolFileReply::Transfer {
                transfer: "ab".repeat(32),
            }
        },
    );
    assert!(
        hs.sent()
            .iter()
            .all(|c| !matches!(c, ServiceCommand::GuestTools { run_id: r, .. } if *r == run_id(2)))
    );
}

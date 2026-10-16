//! F2 use cases of the core: media import, VM deletion, console ticket
//! audit. Fake ports (no files, no QEMU).

mod common;

use common::*;
use vm_core::proto::*;

fn code<T: std::fmt::Debug>(r: Result<T, PublicError>) -> ErrorCode {
    match r {
        Err(e) => e.code,
        Ok(v) => panic!("expected an error, got {v:?}"),
    }
}

fn last(h: &Harness, action: AuditAction) -> AuditEvent {
    h.audit_events()
        .into_iter()
        .rev()
        .find(|e| e.action == action)
        .unwrap_or_else(|| panic!("no {action:?} event"))
}

#[test]
fn stopped_vm_is_deleted_and_audited() {
    let mut h = Harness::new();
    let vm = h.create_vm("to-delete");
    let run = h.start(vm);
    h.stop(vm);
    h.core.delete_vm(&Caller::User, vm, 1).unwrap();
    assert_eq!(
        code(h.core.vm_status(&Caller::User, vm)),
        ErrorCode::NotFound
    );
    assert!(h.store.0.borrow().vms.is_empty());
    assert!(!h.store.0.borrow().runs.contains_key(&run));
    let e = last(&h, AuditAction::VmDelete);
    assert_eq!(e.outcome, AuditOutcome::Succeeded);
    assert_eq!(e.vm_id, Some(vm));
    // A second deletion finds nothing.
    assert_eq!(
        code(h.core.delete_vm(&Caller::User, vm, 1)),
        ErrorCode::NotFound
    );
}

#[test]
fn deletion_is_refused_while_running_busy_or_stale() {
    let mut h = Harness::new();
    let vm = h.create_vm("busy");
    let run = h.start(vm);
    assert_eq!(
        code(h.core.delete_vm(&Caller::User, vm, 1)),
        ErrorCode::StateConflict
    );
    assert_eq!(
        last(&h, AuditAction::VmDelete).outcome,
        AuditOutcome::Denied
    );
    h.stop(vm);
    let _ = run;
    // Queued start not yet executed: operations in progress.
    let r = h.req(vm, None, VmAction::Start);
    h.core.submit(&Caller::User, r).unwrap();
    assert_eq!(
        code(h.core.delete_vm(&Caller::User, vm, 1)),
        ErrorCode::VmBusy
    );
    h.drain(vm);
    h.stop(vm);
    // Wrong revision.
    assert_eq!(
        code(h.core.delete_vm(&Caller::User, vm, 7)),
        ErrorCode::StateConflict
    );
    // Agent session, even with every capability on the VM.
    let (_, agent) = h.session(&[vm], &[Capability::VmStart, Capability::VmStop], 3);
    assert_eq!(
        code(h.core.delete_vm(&agent, vm, 1)),
        ErrorCode::PermissionDenied
    );
    // Catalog failure: the VM stays, the failure is audited.
    h.store.0.borrow_mut().fail_files = true;
    assert_eq!(
        code(h.core.delete_vm(&Caller::User, vm, 1)),
        ErrorCode::InfrastructureError
    );
    assert_eq!(
        last(&h, AuditAction::VmDelete).outcome,
        AuditOutcome::Failed
    );
    assert!(h.core.vm_status(&Caller::User, vm).is_ok());
    h.store.0.borrow_mut().fail_files = false;
    h.core.delete_vm(&Caller::User, vm, 1).unwrap();
}

#[test]
fn media_import_is_user_only_and_audited_without_the_path() {
    let mut h = Harness::new();
    let origin = Label::parse("ubuntu.com").unwrap();
    let m = h
        .core
        .import_media(
            &Caller::User,
            std::path::Path::new("C:/Users/u/Downloads/ubuntu.iso"),
            MediaKind::InstallIso,
            origin.clone(),
        )
        .unwrap();
    assert_eq!(m.kind, MediaKind::InstallIso);
    assert!(
        h.core
            .list_media(&Caller::User)
            .unwrap()
            .iter()
            .any(|r| r.id == m.id)
    );
    let e = last(&h, AuditAction::MediaImport);
    assert_eq!(e.outcome, AuditOutcome::Succeeded);
    assert!(!serde_json::to_string(&e).unwrap().contains("Downloads"));
    // The imported medium can be used as installation media.
    let cfg = h.vm_config("from-import", Some(m.id));
    h.core.create_vm(&Caller::User, cfg).unwrap();

    let vm = h.create_vm("x");
    let (_, agent) = h.session(&[vm], &[Capability::VmStatus], 4);
    assert_eq!(
        code(h.core.import_media(
            &agent,
            std::path::Path::new("a.iso"),
            MediaKind::DataIso,
            origin.clone()
        )),
        ErrorCode::PermissionDenied
    );
    assert_eq!(
        last(&h, AuditAction::MediaImport).outcome,
        AuditOutcome::Denied
    );
    assert_eq!(code(h.core.list_media(&agent)), ErrorCode::PermissionDenied);
    h.store.0.borrow_mut().fail_files = true;
    assert_eq!(
        code(h.core.import_media(
            &Caller::User,
            std::path::Path::new("a.iso"),
            MediaKind::DataIso,
            origin
        )),
        ErrorCode::InfrastructureError
    );
    assert_eq!(
        last(&h, AuditAction::MediaImport).outcome,
        AuditOutcome::Failed
    );
}

#[test]
fn media_deletion_is_user_only_refused_while_used_and_audited() {
    let mut h = Harness::new();
    let origin = Label::parse("ubuntu.com").unwrap();
    let path = std::path::Path::new("C:/Users/u/Downloads/ubuntu.iso");
    let m = h
        .core
        .import_media(&Caller::User, path, MediaKind::InstallIso, origin)
        .unwrap();
    let a = h
        .core
        .create_vm(&Caller::User, h.vm_config("web-test", Some(m.id)))
        .unwrap()
        .id;
    h.core
        .create_vm(&Caller::User, h.vm_config("api-test", Some(m.id)))
        .unwrap();

    // Used by two VMs.
    let e = h.core.delete_media(&Caller::User, m.id).unwrap_err();
    assert_eq!(e.code, ErrorCode::StateConflict);
    assert!(
        e.message.starts_with(
            "the medium is used by 2 VMs: \u{201c}api-test\u{201d}, \u{201c}web-test\u{201d}"
        ),
        "{}",
        e.message
    );
    assert_eq!(
        e.details
            .as_ref()
            .and_then(|d| d.get("reason"))
            .map(String::as_str),
        Some("media_in_use")
    );
    assert_eq!(
        last(&h, AuditAction::MediaDelete).outcome,
        AuditOutcome::Denied
    );
    assert!(h.store.0.borrow().media.contains_key(&m.id));

    // Agent session: denied whatever its capabilities.
    let (_, agent) = h.session(&[a], &[Capability::VmStatus], 4);
    assert_eq!(
        code(h.core.delete_media(&agent, m.id)),
        ErrorCode::PermissionDenied
    );

    // Once no VM uses it: catalog failure audited, then deleted.
    let vms: Vec<VmId> = h.store.0.borrow().vms.keys().copied().collect();
    for vm in vms {
        h.core.delete_vm(&Caller::User, vm, 1).unwrap();
    }
    h.store.0.borrow_mut().fail_files = true;
    assert_eq!(
        code(h.core.delete_media(&Caller::User, m.id)),
        ErrorCode::InfrastructureError
    );
    assert_eq!(
        last(&h, AuditAction::MediaDelete).outcome,
        AuditOutcome::Failed
    );
    h.store.0.borrow_mut().fail_files = false;
    h.core.delete_media(&Caller::User, m.id).unwrap();
    let e = last(&h, AuditAction::MediaDelete);
    assert_eq!(e.outcome, AuditOutcome::Succeeded);
    assert!(
        !h.core
            .list_media(&Caller::User)
            .unwrap()
            .iter()
            .any(|r| r.id == m.id)
    );
    assert_eq!(
        code(h.core.delete_media(&Caller::User, m.id)),
        ErrorCode::NotFound
    );
}

#[test]
fn console_ticket_requires_a_running_vm_and_is_audited() {
    let mut h = Harness::new();
    let vm = h.create_vm("console");
    assert_eq!(
        code(h.core.open_console(&Caller::User, vm)),
        ErrorCode::StateConflict
    );
    let run = h.start(vm);
    let st = h.core.open_console(&Caller::User, vm).unwrap();
    assert_eq!(st.run_id, Some(run));
    // Not audited as succeeded until the ticket is actually issued.
    assert!(
        !h.audit_events()
            .iter()
            .any(|e| e.action == AuditAction::ConsoleOpen && e.outcome == AuditOutcome::Succeeded)
    );
    // Proxy not started: failed with the code.
    h.core
        .record_console_open(&Caller::User, vm, run, Err(ErrorCode::InfrastructureError));
    let e = last(&h, AuditAction::ConsoleOpen);
    assert_eq!(e.outcome, AuditOutcome::Failed);
    assert_eq!(e.error, Some(ErrorCode::InfrastructureError));
    h.core.record_console_open(&Caller::User, vm, run, Ok(()));
    let e = last(&h, AuditAction::ConsoleOpen);
    assert_eq!(e.outcome, AuditOutcome::Succeeded);
    assert_eq!(e.run_id, Some(run));
    // Revocation with its cause.
    h.core
        .record_console_revoke(Actor::System, vm, run, TerminationCause::VmTerminated);
    let e = last(&h, AuditAction::ConsoleRevoke);
    assert_eq!(e.outcome, AuditOutcome::Succeeded);
    assert_eq!((e.vm_id, e.run_id), (Some(vm), Some(run)));
    assert_eq!(e.termination, Some(TerminationCause::VmTerminated));
    let (_, agent) = h.session(&[vm], &[Capability::VmScreenshot], 5);
    assert_eq!(
        code(h.core.open_console(&agent, vm)),
        ErrorCode::PermissionDenied
    );
}

#[test]
fn user_input_is_accepted_while_stopping_agents_and_stopped_vms_are_refused() {
    use vm_core::{action_allowed_in, input_allowed_for};
    let user = Caller::User;
    let agent = Caller::Agent {
        session_id: SessionId::from_unix_millis(1, [1; 10]),
        token: None,
    };
    for s in [
        VmState::Stopped,
        VmState::Starting,
        VmState::Running,
        VmState::Stopping,
        VmState::Failed,
        VmState::Recovering,
    ] {
        assert_eq!(
            input_allowed_for(&user, s),
            matches!(s, VmState::Running | VmState::Stopping),
            "{s:?}"
        );
        assert_eq!(input_allowed_for(&agent, s), s == VmState::Running, "{s:?}");
        assert_eq!(
            input_allowed_for(&agent, s),
            action_allowed_in(OperationKind::Input, s)
        );
    }

    let mut h = Harness::new();
    let vm = h.create_vm("stopping-input");
    let run = h.start(vm);
    h.screenshot_geometry(vm, run);
    // Graceful stop forwarded and not finished: VM `stopping`.
    let r = h.req(vm, None, VmAction::Stop);
    h.core.submit(&Caller::User, r).unwrap();
    let stop = h.core.begin_dispatch(vm).unwrap().expect("stop forwarded");
    assert_eq!(
        h.core.vm_status(&Caller::User, vm).unwrap().state,
        VmState::Stopping
    );
    // The user answers the guest's confirmation: forwarded alongside the stop.
    let input = h.input(vm, run, 2);
    let r = h.req(vm, Some(run), input);
    h.core.submit(&Caller::User, r).unwrap();
    let d = h.core.begin_dispatch(vm).unwrap().expect("input forwarded");
    let rec = h
        .core
        .execute_dispatch(d, &mut h.backend, &mut h.inspector)
        .unwrap();
    assert_eq!(rec.status, OperationStatus::Succeeded);
    // Stale run: refused.
    let other = RunId::from_unix_millis(1, [9; 10]);
    let input = h.input(vm, other, 1);
    let r = h.req(vm, Some(other), input);
    assert_eq!(
        code(h.core.submit(&Caller::User, r)),
        ErrorCode::StateConflict
    );
    // Agent session with the input capability and the input control:
    // refused while stopping.
    let (sid, agent) = h.session(&[vm], &[Capability::VmInput], 6);
    h.core
        .set_input_owner(Actor::User, vm, ipc::InputOwner::Agent { session_id: sid })
        .unwrap();
    let input = h.input(vm, run, 1);
    let r = h.req(vm, Some(run), input);
    assert_eq!(code(h.core.submit(&agent, r)), ErrorCode::StateConflict);
    // End of the stop: no process, input refused (control back to the user).
    h.core
        .set_input_owner(Actor::User, vm, ipc::InputOwner::User)
        .unwrap();
    h.core
        .finish_dispatch(vm, stop.operation_id, Ok(vm_core::DispatchOutcome::Done))
        .unwrap();
    let input = h.input(vm, run, 1);
    let r = h.req(vm, Some(run), input);
    assert_eq!(
        code(h.core.submit(&Caller::User, r)),
        ErrorCode::StateConflict
    );
}

/// A graceful stop whose escalation did not observe the exit in time leaves
/// the VM `stopping` with the run current; the exit observed later closes
/// the run as an ordinary stop.
#[test]
fn graceful_stop_timeout_stays_stopping_until_the_exit_is_observed() {
    let mut h = Harness::new();
    let vm = h.create_vm("slow-stop");
    let run = h.start(vm);
    let r = h.req(vm, None, VmAction::Stop);
    h.core.submit(&Caller::User, r).unwrap();
    let stop = h.core.begin_dispatch(vm).unwrap().expect("stop forwarded");
    let rec = h
        .core
        .finish_dispatch(
            vm,
            stop.operation_id,
            Err(PublicError::infrastructure("QEMU shutdown")),
        )
        .unwrap();
    assert_eq!(rec.status, OperationStatus::Failed);
    let st = h.core.vm_status(&Caller::User, vm).unwrap();
    assert_eq!((st.state, st.run_id), (VmState::Stopping, Some(run)));
    // A forced stop is still admitted (queued: the exit below comes first).
    let r = h.req(vm, None, VmAction::ForceStop);
    assert!(h.core.submit(&Caller::User, r).is_ok());
    // The exit is observed (escalation kill, unexpected for the backend).
    assert!(
        h.core
            .observe(
                vm,
                BackendObservation::ProcessExited {
                    run_id: run,
                    expected: false,
                },
            )
            .unwrap()
    );
    let st = h.core.vm_status(&Caller::User, vm).unwrap();
    assert_eq!((st.state, st.run_id), (VmState::Stopped, None));
}

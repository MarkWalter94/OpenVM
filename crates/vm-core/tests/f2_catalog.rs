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
    let e = last(&h, AuditAction::ConsoleOpen);
    assert_eq!(e.outcome, AuditOutcome::Succeeded);
    assert_eq!(e.run_id, Some(run));
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
    // Agent session with the input capability: refused while stopping.
    let (_, agent) = h.session(&[vm], &[Capability::VmInput], 6);
    let input = h.input(vm, run, 1);
    let r = h.req(vm, Some(run), input);
    assert_eq!(code(h.core.submit(&agent, r)), ErrorCode::StateConflict);
    // End of the stop: no process, input refused.
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

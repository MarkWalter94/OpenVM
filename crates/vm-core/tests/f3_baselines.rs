//! F3 use cases of the core: long storage operations (baseline creation and
//! restore) outside the per-VM queue, baseline deletion, start refused
//! before the backend. Fake ports (no files, no QEMU).

mod common;

use common::*;
use vm_core::proto::*;

fn code<T: std::fmt::Debug>(r: Result<T, PublicError>) -> ErrorCode {
    match r {
        Err(e) => e.code,
        Ok(v) => panic!("expected an error, got {v:?}"),
    }
}

fn outcomes(h: &Harness, action: AuditAction) -> Vec<AuditOutcome> {
    h.audit_events()
        .into_iter()
        .filter(|e| e.action == action)
        .map(|e| e.outcome)
        .collect()
}

#[test]
fn storage_operation_keeps_the_vm_busy_until_it_ends() {
    let mut h = Harness::new();
    let vm = h.create_vm("baseline");
    let (op, ()) = h
        .core
        .begin_storage_operation(&Caller::User, vm, OperationKind::Snapshot, |_, rec, _| {
            assert_eq!(rec.id, vm);
            Ok(())
        })
        .unwrap();
    let st = h.core.vm_status(&Caller::User, vm).unwrap();
    assert_eq!(st.storage_operation, Some(OperationKind::Snapshot));
    // Every other mutation is busy.
    let start = h.req(vm, None, VmAction::Start);
    assert_eq!(code(h.core.submit(&Caller::User, start)), ErrorCode::VmBusy);
    assert_eq!(
        code(h.core.begin_storage_operation(
            &Caller::User,
            vm,
            OperationKind::Restore,
            |_, _, _| Ok(())
        )),
        ErrorCode::VmBusy
    );
    assert_eq!(
        code(h.core.delete_vm(&Caller::User, vm, 1)),
        ErrorCode::VmBusy
    );
    let cfg = h.vm_config("baseline", None);
    assert_eq!(
        code(h.core.update_vm_config(&Caller::User, vm, 1, cfg)),
        ErrorCode::VmBusy
    );
    assert_eq!(
        code(h.core.delete_baseline_with(&Caller::User, vm, |_| Ok(()))),
        ErrorCode::VmBusy
    );
    h.core
        .finish_storage_operation(vm, op, |_| Ok::<(), PublicError>(()))
        .unwrap();
    assert_eq!(
        h.core
            .vm_status(&Caller::User, vm)
            .unwrap()
            .storage_operation,
        None
    );
    assert_eq!(
        outcomes(&h, AuditAction::BaselineCreate),
        vec![AuditOutcome::Accepted, AuditOutcome::Succeeded]
    );
    // Free again: a start is accepted.
    h.start(vm);
}

#[test]
fn storage_operations_require_a_stopped_vm_and_the_user() {
    let mut h = Harness::new();
    let vm = h.create_vm("guarded");
    let run = h.start(vm);
    assert_eq!(
        code(h.core.begin_storage_operation(
            &Caller::User,
            vm,
            OperationKind::Restore,
            |_, _, _| Ok(())
        )),
        ErrorCode::StateConflict
    );
    h.stop(vm);
    let _ = run;
    let (_, agent) = h.session(&[vm], &[Capability::VmSnapshot, Capability::VmRestore], 7);
    assert_eq!(
        code(
            h.core
                .begin_storage_operation(&agent, vm, OperationKind::Snapshot, |_, _, _| Ok(()))
        ),
        ErrorCode::PermissionDenied
    );
    assert_eq!(
        code(h.core.delete_baseline_with(&agent, vm, |_| Ok(()))),
        ErrorCode::PermissionDenied
    );
    assert_eq!(
        code(h.core.begin_storage_operation(
            &Caller::User,
            vm,
            OperationKind::Start,
            |_, _, _| Ok(())
        )),
        ErrorCode::InvalidArgument
    );
    // A failure of the catalog step is audited and leaves the VM free.
    assert_eq!(
        code(h.core.begin_storage_operation(
            &Caller::User,
            vm,
            OperationKind::Snapshot,
            |_, _, _| Err::<(), _>(PublicError::state_conflict("unclean"))
        )),
        ErrorCode::StateConflict
    );
    assert_eq!(
        h.core
            .vm_status(&Caller::User, vm)
            .unwrap()
            .storage_operation,
        None
    );
    let o = outcomes(&h, AuditAction::BaselineCreate);
    assert!(o.contains(&AuditOutcome::Denied) && o.contains(&AuditOutcome::Failed));
}

#[test]
fn successful_restore_closes_the_control_sessions_of_the_vm() {
    let mut h = Harness::new();
    let vm = h.create_vm("restored");
    let (sid, _) = h.session(&[vm], &[Capability::VmStatus], 9);
    let (op, ()) = h
        .core
        .begin_storage_operation(&Caller::User, vm, OperationKind::Restore, |_, _, _| Ok(()))
        .unwrap();
    h.core
        .finish_storage_operation(vm, op, |_| Ok::<(), PublicError>(()))
        .unwrap();
    assert!(h.core.sessions().live_for_vm(vm).is_empty());
    let terminated = h.audit_events().into_iter().any(|e| {
        e.action == AuditAction::SessionTerminate
            && e.session_id == Some(sid)
            && e.termination == Some(TerminationCause::Restored)
    });
    assert!(terminated);
    // A failed restore does not.
    let (sid2, _) = h.session(&[vm], &[Capability::VmStatus], 10);
    let (op, ()) = h
        .core
        .begin_storage_operation(&Caller::User, vm, OperationKind::Restore, |_, _, _| Ok(()))
        .unwrap();
    let _ = h.core.finish_storage_operation(vm, op, |_| {
        Err::<(), _>(PublicError::infrastructure("qemu-img failed"))
    });
    assert!(h.core.sessions().live_for_vm(vm).contains(&sid2));
    assert_eq!(
        outcomes(&h, AuditAction::BaselineRestore).last(),
        Some(&AuditOutcome::Failed)
    );
}

#[test]
fn start_refused_before_the_backend_leaves_the_vm_stopped() {
    let mut h = Harness::new();
    let vm = h.create_vm("refused");
    let req = h.req(vm, None, VmAction::Start);
    let op = req.operation_id;
    h.core.submit(&Caller::User, req).unwrap();
    let d = h.core.begin_dispatch(vm).unwrap().unwrap();
    assert_eq!(d.operation_id, op);
    let rec = h
        .core
        .refuse_start(
            vm,
            op,
            PublicError::state_conflict("baseline altered").with_detail("reason", "integrity"),
        )
        .unwrap();
    assert_eq!(rec.status, OperationStatus::Failed);
    assert_eq!(rec.error, Some(ErrorCode::StateConflict));
    let st = h.core.vm_status(&Caller::User, vm).unwrap();
    assert_eq!(st.state, VmState::Stopped);
    assert_eq!(st.run_id, None);
    assert_eq!(h.backend.count(|c| matches!(c, Call::Start(..))), 0);
    // The VM can be started normally afterwards.
    h.start(vm);
}

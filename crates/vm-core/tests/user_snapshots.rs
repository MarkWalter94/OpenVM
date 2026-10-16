//! User snapshots in the core (ADR-0032): user only, refused while an
//! agent session targets the VM, take and delete with the VM off or running,
//! revert only with the VM off, the VM busy in between, the reverted
//! configuration becomes the core's, audit of every step. Fake ports.

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
fn take_off_and_running_delete_running_revert_off_only() {
    let mut h = Harness::new();
    let vm = h.create_vm("snap");
    // Off.
    let (op, run) = h
        .core
        .begin_snapshot_operation(
            &Caller::User,
            vm,
            OperationKind::UserSnapshot,
            |_, rec, run| {
                assert_eq!(rec.id, vm);
                Ok(run)
            },
        )
        .unwrap();
    assert_eq!(run, None);
    let st = h.core.vm_status(&Caller::User, vm).unwrap();
    assert_eq!(st.storage_operation, Some(OperationKind::UserSnapshot));
    // Busy meanwhile.
    let start = h.req(vm, None, VmAction::Start);
    assert_eq!(code(h.core.submit(&Caller::User, start)), ErrorCode::VmBusy);
    assert_eq!(
        code(h.core.begin_snapshot_operation(
            &Caller::User,
            vm,
            OperationKind::UserSnapshotDelete,
            |_, _, _| Ok(())
        )),
        ErrorCode::VmBusy
    );
    h.core
        .finish_snapshot_operation(vm, op, |_| Ok(((), None)))
        .unwrap();
    // Running: take and delete accepted with the current run, revert refused.
    let run = h.start(vm);
    let (op, got) = h
        .core
        .begin_snapshot_operation(&Caller::User, vm, OperationKind::UserSnapshot, |_, _, r| {
            Ok(r)
        })
        .unwrap();
    assert_eq!(got, Some(run));
    h.core
        .finish_snapshot_operation(vm, op, |_| Ok(((), None)))
        .unwrap();
    let (op, _) = h
        .core
        .begin_snapshot_operation(
            &Caller::User,
            vm,
            OperationKind::UserSnapshotDelete,
            |_, _, _| Ok(()),
        )
        .unwrap();
    h.core
        .finish_snapshot_operation(vm, op, |_| Ok(((), None)))
        .unwrap();
    let e = h
        .core
        .begin_snapshot_operation(
            &Caller::User,
            vm,
            OperationKind::UserSnapshotRevert,
            |_, _, _| Ok(()),
        )
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::StateConflict);
    assert!(e.message.contains("shut it down"), "{}", e.message);
    // Not a snapshot kind.
    assert_eq!(
        code(h.core.begin_snapshot_operation(
            &Caller::User,
            vm,
            OperationKind::Start,
            |_, _, _| Ok(())
        )),
        ErrorCode::InvalidArgument
    );
    assert_eq!(
        outcomes(&h, AuditAction::SnapshotTake),
        vec![
            AuditOutcome::Accepted,
            AuditOutcome::Succeeded,
            AuditOutcome::Accepted,
            AuditOutcome::Succeeded
        ]
    );
    assert_eq!(
        outcomes(&h, AuditAction::SnapshotRevert),
        vec![AuditOutcome::Denied]
    );
}

#[test]
fn agents_and_agent_sessions_are_refused() {
    let mut h = Harness::new();
    let vm = h.create_vm("guarded");
    let (_sid, agent) = h.session(&[vm], &[Capability::VmStatus], 1);
    // The agent itself: permission denied, for operations and reads.
    for kind in [
        OperationKind::UserSnapshot,
        OperationKind::UserSnapshotRevert,
        OperationKind::UserSnapshotDelete,
    ] {
        assert_eq!(
            code(
                h.core
                    .begin_snapshot_operation(&agent, vm, kind, |_, _, _| Ok(()))
            ),
            ErrorCode::PermissionDenied
        );
    }
    assert_eq!(
        code(h.core.snapshot_access(&agent, vm, false, |_| Ok(()))),
        ErrorCode::PermissionDenied
    );
    // The user, while the session targets the VM: state conflict.
    let e = h
        .core
        .begin_snapshot_operation(&Caller::User, vm, OperationKind::UserSnapshot, |_, _, _| {
            Ok(())
        })
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::StateConflict);
    assert_eq!(
        e.details
            .as_ref()
            .and_then(|d| d.get("reason"))
            .map(String::as_str),
        Some("agent_session")
    );
    assert_eq!(
        h.core.snapshot_unavailable_reason(vm),
        Some(vm_core::SNAPSHOT_AGENT_SESSION)
    );
    // Reading and renaming stay available to the user.
    h.core
        .snapshot_access(&Caller::User, vm, true, |_| Ok(()))
        .unwrap();
    assert_eq!(
        outcomes(&h, AuditAction::SnapshotTake),
        vec![AuditOutcome::Denied, AuditOutcome::Denied]
    );
    assert_eq!(
        outcomes(&h, AuditAction::SnapshotUpdate),
        vec![AuditOutcome::Denied, AuditOutcome::Succeeded]
    );
}

#[test]
fn revert_record_becomes_the_core_record() {
    let mut h = Harness::new();
    let vm = h.create_vm("revert");
    let rec = h.core.vm_status(&Caller::User, vm).unwrap();
    let mut old = h.vm_config("other-name", None);
    old.vcpus = 1;
    old.memory_mib = 1024;
    // The name stays the VM's; revision + 1.
    let new = h.core.reverted_record(vm, old.clone()).unwrap();
    assert_eq!(new.config.name, rec.config.name);
    assert_eq!(new.config.vcpus, 1);
    assert_eq!(new.config_revision, rec.config_revision + 1);
    // Media gone from the registry are detached.
    let mut with_media = old.clone();
    with_media.install_media = Some(MediaId::from_unix_millis(1, [7; 10]));
    assert_eq!(
        h.core
            .reverted_record(vm, with_media)
            .unwrap()
            .config
            .install_media,
        None
    );
    // Beyond the current limits: refused.
    let mut too_big = old;
    too_big.vcpus = 4096;
    let e = h.core.reverted_record(vm, too_big).unwrap_err();
    assert_eq!(
        e.details
            .as_ref()
            .and_then(|d| d.get("reason"))
            .map(String::as_str),
        Some("snapshot_config")
    );
    let (op, ()) = h
        .core
        .begin_snapshot_operation(
            &Caller::User,
            vm,
            OperationKind::UserSnapshotRevert,
            |_, _, _| Ok(()),
        )
        .unwrap();
    let applied = new.clone();
    h.core
        .finish_snapshot_operation(vm, op, move |_| Ok(((), Some(applied))))
        .unwrap();
    let st = h.core.vm_status(&Caller::User, vm).unwrap();
    assert_eq!(st.config.vcpus, 1);
    assert_eq!(st.config_revision, new.config_revision);
    // A failed finish leaves the record alone and is audited as failed.
    let (op, ()) = h
        .core
        .begin_snapshot_operation(
            &Caller::User,
            vm,
            OperationKind::UserSnapshotRevert,
            |_, _, _| Ok(()),
        )
        .unwrap();
    let r: Result<(), _> = h.core.finish_snapshot_operation(vm, op, |_| {
        Err(PublicError::state_conflict("disk not reverted"))
    });
    assert!(r.is_err());
    assert_eq!(
        h.core.vm_status(&Caller::User, vm).unwrap().config_revision,
        new.config_revision
    );
    assert_eq!(
        outcomes(&h, AuditAction::SnapshotRevert),
        vec![
            AuditOutcome::Accepted,
            AuditOutcome::Succeeded,
            AuditOutcome::Accepted,
            AuditOutcome::Failed
        ]
    );
}

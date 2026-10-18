//! Operation records and identifiers: agent storage operations counted and
//! visible to their session, `operation` lookups that do not reveal other
//! VMs, the operation index bounded beyond the idempotency window. Fake
//! ports (no files, no QEMU).

mod common;

use common::*;
use vm_core::proto::*;
use vm_core::session::SessionSpec;
use vm_core::{IDEMPOTENCY_WINDOW, RETIRED_OPERATION_IDS};

fn code<T: std::fmt::Debug>(r: Result<T, PublicError>) -> ErrorCode {
    match r {
        Err(e) => e.code,
        Ok(v) => panic!("expected an error, got {v:?}"),
    }
}

fn with_baselines() -> Harness {
    let mut cfg = config();
    cfg.host.baseline_available = true;
    Harness::with_config(cfg)
}

/// Session on `vm` with snapshot, restore and status and at most
/// `max_operations` operations.
fn limited_session(
    h: &mut Harness,
    vm: VmId,
    max_operations: u32,
    token: u8,
) -> (SessionId, Caller) {
    let spec = SessionSpec {
        tools: Default::default(),
        vm_allowlist: [vm].into_iter().collect(),
        capabilities: [
            Capability::VmStatus,
            Capability::VmSnapshot,
            Capability::VmRestore,
        ]
        .into_iter()
        .collect(),
        quotas: Quotas {
            max_operations,
            ..Quotas::UNBOUNDED
        },
        expires_at: UtcTimestamp::from_unix_millis(h.clock.0.get() + 3_600_000).unwrap(),
        lease_millis: 10_000,
        network_endpoints: Default::default(),
    };
    let token = SessionToken::from_bytes([token; 32]);
    let (grant, _) = h.core.create_session(spec, token.clone()).unwrap();
    (
        grant.session_id,
        Caller::Agent {
            session_id: grant.session_id,
            token: Some(token),
        },
    )
}

#[test]
fn agent_storage_operations_count_against_the_quota_and_are_its_own() {
    let mut h = with_baselines();
    let vm = h.create_vm("agent-storage");
    let (sid, agent) = limited_session(&mut h, vm, 2, 3);

    let (op, ()) = h
        .core
        .begin_storage_operation(&agent, vm, OperationKind::Snapshot, |_, _, _| Ok(()))
        .unwrap();
    assert_eq!(h.core.sessions().get(sid).unwrap().usage.operations, 1);
    // In progress: the session sees its operation.
    let r = h.core.operation(&agent, op).unwrap();
    assert_eq!(r.session_id, Some(sid));
    assert_eq!(r.status, OperationStatus::Forwarded);
    h.core
        .finish_storage_operation(vm, op, |_| Ok::<(), PublicError>(()))
        .unwrap();
    // Finished: still readable by the session and by the user.
    assert_eq!(
        h.core.operation(&agent, op).unwrap().status,
        OperationStatus::Succeeded
    );
    assert_eq!(
        h.core.operation(&Caller::User, op).unwrap().session_id,
        Some(sid)
    );

    let (op2, ()) = h
        .core
        .begin_storage_operation(&agent, vm, OperationKind::Restore, |_, _, _| Ok(()))
        .unwrap();
    h.core
        .finish_storage_operation(vm, op2, |_| Ok::<(), PublicError>(()))
        .unwrap();
    assert_eq!(h.core.sessions().get(sid).unwrap().usage.operations, 2);
    // Quota exhausted: refused before the catalog step, audited as denied.
    let mut ran = false;
    assert_eq!(
        code(
            h.core
                .begin_storage_operation(&agent, vm, OperationKind::Snapshot, |_, _, _| {
                    ran = true;
                    Ok(())
                })
        ),
        ErrorCode::ResourceLimit
    );
    assert!(!ran);
    assert!(h.audit_events().iter().any(|e| {
        e.action == AuditAction::BaselineCreate
            && e.outcome == AuditOutcome::Denied
            && e.error == Some(ErrorCode::ResourceLimit)
    }));
    // The user's storage operations are not counted for any session.
    let (op3, ()) = h
        .core
        .begin_storage_operation(&Caller::User, vm, OperationKind::Snapshot, |_, _, _| Ok(()))
        .unwrap();
    assert_eq!(
        h.core.operation(&Caller::User, op3).unwrap().session_id,
        None
    );
    assert_eq!(code(h.core.operation(&agent, op3)), ErrorCode::NotFound);
}

#[test]
fn an_operation_of_a_vm_outside_the_grant_is_not_found() {
    let mut h = Harness::new();
    let mine = h.create_vm("mine");
    let theirs = h.create_vm("theirs");
    let (_, agent) = h.session(&[mine], &[Capability::VmStatus], 5);
    let req = h.req(theirs, None, VmAction::Start);
    let op = req.operation_id;
    h.core.submit(&Caller::User, req).unwrap();
    let unknown = h.op();
    // Same answer for an id of another VM and an id that does not exist.
    assert_eq!(code(h.core.operation(&agent, op)), ErrorCode::NotFound);
    assert_eq!(code(h.core.operation(&agent, unknown)), ErrorCode::NotFound);
    assert!(h.core.operation(&Caller::User, op).is_ok());
}

#[test]
fn operation_ids_stay_reserved_for_a_bounded_time_after_the_window() {
    let mut h = Harness::new();
    let vm = h.create_vm("many");
    let run = h.start(vm);
    let shot = |h: &mut Harness| {
        let req = h.req(vm, Some(run), VmAction::Screenshot);
        let id = req.operation_id;
        h.core.submit(&Caller::User, req).unwrap();
        h.run_next(vm).unwrap();
        id
    };
    let first = shot(&mut h);
    let mut ids = vec![first];
    for _ in 0..(IDEMPOTENCY_WINDOW + RETIRED_OPERATION_IDS) {
        ids.push(shot(&mut h));
    }
    // `first` (and the start) left the window and then the reserve: the id
    // is released.
    let reuse = |h: &mut Harness, id: OperationId| {
        h.core.submit(
            &Caller::User,
            VmRequest {
                operation_id: id,
                vm_id: vm,
                expected_run_id: Some(run),
                action: VmAction::Screenshot,
            },
        )
    };
    // An id retired from the window but still in the reserve is refused,
    // even for the same request.
    let reserved = ids[ids.len() - IDEMPOTENCY_WINDOW - 1];
    assert_eq!(code(reuse(&mut h, reserved)), ErrorCode::InvalidArgument);
    // `first` left the window and then the reserve: the id is released.
    assert!(!reuse(&mut h, first).unwrap().duplicate);
    h.run_next(vm).unwrap();
    // Operations still in the window remain readable and idempotent.
    let last = *ids.last().unwrap();
    assert_eq!(
        h.core.operation(&Caller::User, last).unwrap().status,
        OperationStatus::Succeeded
    );
    let dup = h
        .core
        .submit(
            &Caller::User,
            VmRequest {
                operation_id: last,
                vm_id: vm,
                expected_run_id: Some(run),
                action: VmAction::Screenshot,
            },
        )
        .unwrap();
    assert!(dup.duplicate);
}

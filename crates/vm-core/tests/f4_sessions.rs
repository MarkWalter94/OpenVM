//! F4 (bridge and sessions) at the core level, with fake ports: every
//! protected call verified twice (receipt and right before forwarding) for
//! every kind of caller, input ownership, termination rules and audit of the
//! agent calls (ROADMAP 7.3, 7.4; ADR-0026).

mod common;

use common::*;
use vm_core::CoreConfig;
use vm_core::proto::ipc::InputOwner;
use vm_core::proto::*;

fn code<T: std::fmt::Debug>(r: Result<T, PublicError>) -> Option<ErrorCode> {
    r.err().map(|e| e.code)
}

/// Configuration with room for several sessions and running VMs (the MVP
/// default allows one of each).
fn roomy() -> CoreConfig {
    let mut c = config();
    c.global_limits.max_agent_sessions = 16;
    c.global_limits.max_running_vms = 4;
    c
}

/// Kind of caller in the matrix.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Who {
    User,
    /// Agent with every capability that owns the input.
    Agent,
    /// Agent with `vm.list` and `vm.status` only.
    AgentNoCap,
    /// Agent whose grant covers another VM.
    AgentOtherVm,
    /// Agent paused by the user.
    AgentPaused,
    /// Agent whose lease expired.
    AgentExpired,
    /// Agent revoked by the user.
    AgentRevoked,
    /// Agent presenting a wrong token.
    AgentWrongToken,
    /// Agent without a token.
    AgentNoToken,
    /// Agent with every capability while the user owns the input.
    AgentNotOwner,
}

const WHO: [Who; 10] = [
    Who::User,
    Who::Agent,
    Who::AgentNoCap,
    Who::AgentOtherVm,
    Who::AgentPaused,
    Who::AgentExpired,
    Who::AgentRevoked,
    Who::AgentWrongToken,
    Who::AgentNoToken,
    Who::AgentNotOwner,
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Act {
    Start,
    Stop,
    ForceStop,
    Reset,
    Screenshot,
    Input,
}

const ACTS: [Act; 6] = [
    Act::Start,
    Act::Stop,
    Act::ForceStop,
    Act::Reset,
    Act::Screenshot,
    Act::Input,
];

fn is_mutation(a: Act) -> bool {
    a != Act::Screenshot
}

/// Expected code (`None` = accepted).
fn expected(who: Who, act: Act) -> Option<ErrorCode> {
    match who {
        Who::User | Who::Agent => None,
        Who::AgentNoCap | Who::AgentOtherVm => Some(ErrorCode::PermissionDenied),
        Who::AgentPaused if is_mutation(act) => Some(ErrorCode::PermissionDenied),
        Who::AgentPaused => None,
        Who::AgentExpired => Some(ErrorCode::SessionExpired),
        Who::AgentRevoked | Who::AgentWrongToken | Who::AgentNoToken => {
            Some(ErrorCode::Unauthenticated)
        }
        Who::AgentNotOwner if act == Act::Input => Some(ErrorCode::PermissionDenied),
        Who::AgentNotOwner => None,
    }
}

struct Setup {
    h: Harness,
    vm: VmId,
    run: Option<RunId>,
    caller: Caller,
}

/// Builds the scenario: VM `a` (stopped for `start`, running otherwise),
/// VM `b` (stopped), the caller in its condition.
fn setup(who: Who, act: Act) -> Setup {
    let mut h = Harness::with_config(roomy());
    let vm = h.create_vm("a");
    let other = h.create_vm("b");
    let run = if act == Act::Start {
        None
    } else {
        let run = h.start(vm);
        h.screenshot_geometry(vm, run);
        Some(run)
    };
    let all = ALL_CAPS;
    let caller = match who {
        Who::User => Caller::User,
        Who::Agent => h.session(&[vm], &all, 1).1,
        Who::AgentNoCap => {
            h.session(&[vm], &[Capability::VmList, Capability::VmStatus], 2)
                .1
        }
        Who::AgentOtherVm => h.session(&[other], &all, 3).1,
        Who::AgentPaused => {
            let (sid, c) = h.session(&[vm], &all, 4);
            h.core.pause_session(sid).unwrap();
            c
        }
        Who::AgentExpired => {
            let (_, c) = h.session(&[vm], &all, 5);
            h.clock.advance(10_001);
            c
        }
        Who::AgentRevoked => {
            let (sid, c) = h.session(&[vm], &all, 6);
            h.core
                .revoke_session(sid, TerminationCause::Revoked)
                .unwrap();
            c
        }
        Who::AgentWrongToken => {
            let (sid, _) = h.session(&[vm], &all, 7);
            Caller::Agent {
                session_id: sid,
                token: Some(SessionToken::from_bytes([0x77; 32])),
            }
        }
        Who::AgentNoToken => {
            let (sid, _) = h.session(&[vm], &all, 8);
            Caller::Agent {
                session_id: sid,
                token: None,
            }
        }
        Who::AgentNotOwner => {
            let (_, c) = h.session(&[vm], &all, 9);
            h.core
                .set_input_owner(Actor::User, vm, InputOwner::User)
                .unwrap();
            c
        }
    };
    Setup { h, vm, run, caller }
}

fn action(s: &Setup, act: Act) -> VmAction {
    match act {
        Act::Start => VmAction::Start,
        Act::Stop => VmAction::Stop,
        Act::ForceStop => VmAction::ForceStop,
        Act::Reset => VmAction::Reset,
        Act::Screenshot => VmAction::Screenshot,
        Act::Input => s.h.input(s.vm, s.run.expect("running"), 2),
    }
}

/// Every action × every caller: the receipt check gives the expected code;
/// a denial has no effect on the backend and is audited as `denied` with the
/// code; an accepted request is forwarded and executed.
#[test]
fn every_protected_call_is_checked_on_receipt_for_every_caller() {
    let mut checked = 0;
    for who in WHO {
        for act in ACTS {
            let mut s = setup(who, act);
            let calls_before = s.h.backend.calls.len();
            let audit_before = s.h.audit_events().len();
            let a = action(&s, act);
            let req = s.h.req(s.vm, s.run, a);
            let op = req.operation_id;
            let got = code(s.h.core.submit(&s.caller, req));
            assert_eq!(got, expected(who, act), "{who:?} × {act:?}");
            match got {
                Some(c) => {
                    assert!(s.h.run_next(s.vm).is_none(), "{who:?} × {act:?}");
                    assert_eq!(
                        s.h.backend.calls.len(),
                        calls_before,
                        "no effect: {who:?} × {act:?}"
                    );
                    let ev = s.h.audit_events();
                    let denial = ev[audit_before..]
                        .iter()
                        .find(|e| e.outcome == AuditOutcome::Denied)
                        .unwrap_or_else(|| panic!("denial audited: {who:?} × {act:?}"));
                    assert_eq!(denial.error, Some(c));
                    assert_eq!(denial.operation_id, Some(op));
                }
                None => {
                    let rec = s.h.run_next(s.vm).expect("forwarded");
                    assert_eq!(rec.id, op);
                    assert_eq!(
                        rec.status,
                        OperationStatus::Succeeded,
                        "{who:?} × {act:?}: {rec:?}"
                    );
                    assert_eq!(s.h.backend.calls.len(), calls_before + 1);
                }
            }
            checked += 1;
        }
    }
    assert_eq!(checked, WHO.len() * ACTS.len());
}

/// Observation calls (`list`, `status`) × every caller.
#[test]
fn observation_calls_are_checked_for_every_caller() {
    for who in WHO {
        let mut s = setup(who, Act::Screenshot);
        let list = s.h.core.list_vms(&s.caller);
        let status = s.h.core.vm_status(&s.caller, s.vm);
        let exp = match who {
            Who::User | Who::Agent | Who::AgentPaused | Who::AgentNotOwner => None,
            // `vm.list`/`vm.status` granted, the VM is in the allowlist.
            Who::AgentNoCap => None,
            Who::AgentOtherVm => Some(ErrorCode::PermissionDenied),
            Who::AgentExpired => Some(ErrorCode::SessionExpired),
            Who::AgentRevoked | Who::AgentWrongToken | Who::AgentNoToken => {
                Some(ErrorCode::Unauthenticated)
            }
        };
        assert_eq!(code(status), exp, "status {who:?}");
        match who {
            // The list of an agent on another VM is filtered, not refused.
            Who::AgentOtherVm => {
                let l = list.unwrap();
                assert!(l.iter().all(|v| v.vm_id != s.vm), "{who:?}");
            }
            Who::User => assert_eq!(list.unwrap().len(), 2),
            Who::Agent | Who::AgentNoCap | Who::AgentPaused | Who::AgentNotOwner => {
                let l = list.unwrap();
                assert_eq!(l.len(), 1, "{who:?}");
                assert_eq!(l[0].vm_id, s.vm);
            }
            _ => assert_eq!(code(list), exp, "list {who:?}"),
        }
    }
}

/// Second check: a request accepted on receipt is discarded right before
/// forwarding if, meanwhile, the session was paused, revoked, expired or
/// lost the input control; nothing reaches the backend.
#[test]
fn second_check_before_forwarding_discards_without_effect() {
    #[derive(Debug, Clone, Copy)]
    enum Change {
        Pause,
        Revoke,
        Expire,
        TakeInput,
    }
    for change in [
        Change::Pause,
        Change::Revoke,
        Change::Expire,
        Change::TakeInput,
    ] {
        let mut h = Harness::with_config(roomy());
        let vm = h.create_vm("a");
        let run = h.start(vm);
        h.screenshot_geometry(vm, run);
        let (sid, agent) = h.session(&[vm], &ALL_CAPS, 1);
        let a = h.input(vm, run, 3);
        let req = h.req(vm, Some(run), a);
        let op = req.operation_id;
        h.core.submit(&agent, req).unwrap();
        let expected = match change {
            Change::Pause => {
                h.core.pause_session(sid).unwrap();
                ErrorCode::PermissionDenied
            }
            Change::Revoke => {
                h.core
                    .revoke_session(sid, TerminationCause::Revoked)
                    .unwrap();
                ErrorCode::Unauthenticated
            }
            Change::Expire => {
                h.clock.advance(10_001);
                ErrorCode::SessionExpired
            }
            Change::TakeInput => {
                h.core
                    .set_input_owner(Actor::User, vm, InputOwner::User)
                    .unwrap();
                ErrorCode::PermissionDenied
            }
        };
        let calls = h.backend.calls.len();
        assert!(h.run_next(vm).is_none(), "{change:?}");
        assert_eq!(h.backend.calls.len(), calls, "{change:?}");
        let rec = h.core.operation(&Caller::User, op).unwrap();
        assert_eq!(rec.status, OperationStatus::Discarded, "{change:?}");
        assert_eq!(rec.error, Some(expected), "{change:?}");
    }
}

/// Input ownership: user OR agent, never both; every handover releases the
/// pressed keys; the end of the owning session gives the control back.
#[test]
fn input_ownership_is_exclusive_and_handover_releases_keys() {
    let mut h = Harness::with_config(roomy());
    let vm = h.create_vm("a");
    let run = h.start(vm);
    h.screenshot_geometry(vm, run);
    assert_eq!(h.core.input_owner(vm), InputOwner::User);
    let (sid, agent) = h.session(&[vm], &[Capability::VmInput], 1);
    // `session()` handed the input over: one release recorded.
    assert_eq!(
        h.core.input_owner(vm),
        InputOwner::Agent { session_id: sid }
    );
    assert_eq!(h.core.take_input_releases(), vec![(vm, run)]);
    // The user cannot inject while the agent owns the input.
    let a = h.input(vm, run, 1);
    let r = h.req(vm, Some(run), a);
    let e = h.core.submit(&Caller::User, r).unwrap_err();
    assert_eq!(e.code, ErrorCode::PermissionDenied);
    assert_eq!(
        e.details
            .as_ref()
            .and_then(|d| d.get("reason"))
            .map(String::as_str),
        Some("input_owned_by_agent")
    );
    // The agent can.
    let a = h.input(vm, run, 1);
    let r = h.req(vm, Some(run), a);
    h.core.submit(&agent, r).unwrap();
    // Queued agent input is discarded when the user takes control back.
    let a = h.input(vm, run, 1);
    let r = h.req(vm, Some(run), a);
    let queued = r.operation_id;
    // (the first one is still queued too: nothing was drained)
    h.core.submit(&agent, r).unwrap();
    h.core
        .set_input_owner(Actor::User, vm, InputOwner::User)
        .unwrap();
    assert_eq!(h.core.take_input_releases(), vec![(vm, run)]);
    let rec = h.core.operation(&Caller::User, queued).unwrap();
    assert_eq!(rec.status, OperationStatus::Discarded);
    assert!(h.run_next(vm).is_none());
    assert_eq!(h.backend.count(|c| matches!(c, Call::Input(..))), 0);
    // The agent cannot inject any more; the user can.
    let a = h.input(vm, run, 1);
    let r = h.req(vm, Some(run), a);
    let e = h.core.submit(&agent, r).unwrap_err();
    assert_eq!(
        e.details
            .as_ref()
            .and_then(|d| d.get("reason"))
            .map(String::as_str),
        Some("input_not_owned")
    );
    let a = h.input(vm, run, 1);
    let r = h.req(vm, Some(run), a);
    h.core.submit(&Caller::User, r).unwrap();
    h.drain(vm);
    // Handing it back and revoking the session: the control returns to the
    // user and the keys are released.
    h.core
        .set_input_owner(Actor::User, vm, InputOwner::Agent { session_id: sid })
        .unwrap();
    let _ = h.core.take_input_releases();
    h.core
        .revoke_session(sid, TerminationCause::Revoked)
        .unwrap();
    assert_eq!(h.core.input_owner(vm), InputOwner::User);
    assert_eq!(h.core.take_input_releases(), vec![(vm, run)]);
    // Setting the same owner again is a no-op (no release).
    h.core
        .set_input_owner(Actor::User, vm, InputOwner::User)
        .unwrap();
    assert!(h.core.take_input_releases().is_empty());
    // Every change is audited as `input_control`.
    let n = h
        .audit_events()
        .iter()
        .filter(|e| e.action == AuditAction::InputControl && e.outcome == AuditOutcome::Succeeded)
        .count();
    assert_eq!(n, 4);
}

/// The input can only be handed to a live session whose grant covers the VM
/// and `vm.input`.
#[test]
fn input_handover_requires_a_matching_live_session() {
    let mut h = Harness::with_config(roomy());
    let vm = h.create_vm("a");
    let other = h.create_vm("b");
    let (no_input, _) = h.session(&[vm], &[Capability::VmScreenshot], 1);
    let (wrong_vm, _) = h.session(&[other], &[Capability::VmInput], 2);
    let (gone, _) = h.session(&[vm], &[Capability::VmScreenshot], 3);
    h.core
        .revoke_session(gone, TerminationCause::Revoked)
        .unwrap();
    for (sid, c) in [
        (no_input, ErrorCode::PermissionDenied),
        (wrong_vm, ErrorCode::PermissionDenied),
        (gone, ErrorCode::NotFound),
    ] {
        let r = h
            .core
            .set_input_owner(Actor::User, vm, InputOwner::Agent { session_id: sid });
        assert_eq!(code(r), Some(c));
        assert_eq!(h.core.input_owner(vm), InputOwner::User);
    }
    let missing = VmId::from_unix_millis(1, [3; 10]);
    assert_eq!(
        code(
            h.core
                .set_input_owner(Actor::User, missing, InputOwner::User)
        ),
        Some(ErrorCode::NotFound)
    );
}

/// Lease and grant expiry terminate the session and plan the Emergency Stop
/// of its running VM; a session already revoked does not stop its VM again.
#[test]
fn expiry_stops_the_vm_but_a_revoked_session_does_not() {
    let mut h = Harness::with_config(roomy());
    let vm = h.create_vm("a");
    let run = h.start(vm);
    let (sid, agent) = h.session(&[vm], &ALL_CAPS, 1);
    // Renewal keeps it alive past the first lease.
    h.clock.advance(6_000);
    h.core
        .renew_lease(sid, &SessionToken::from_bytes([1; 32]), 10_000)
        .unwrap();
    h.clock.advance(6_000);
    assert!(h.core.tick().unwrap().is_empty());
    h.clock.advance(5_000);
    let t = h.core.tick().unwrap();
    assert_eq!(t.len(), 1);
    assert_eq!(t[0].cause, TerminationCause::LeaseExpired);
    assert_eq!(t[0].emergency_stops.len(), 1);
    assert_eq!(t[0].emergency_stops[0].run_id, Some(run));
    h.core
        .finish_emergency_stop(Actor::System, vm, run, Ok(()))
        .unwrap();
    assert_eq!(
        h.core.vm_status(&Caller::User, vm).unwrap().state,
        VmState::Stopped
    );
    let r = h.req(vm, None, VmAction::Start);
    assert_eq!(
        code(h.core.submit(&agent, r)),
        Some(ErrorCode::SessionExpired)
    );
    // Revoked then "lost": no second stop.
    let run = h.start(vm);
    let (sid2, _) = h.session(&[vm], &ALL_CAPS, 2);
    h.core
        .revoke_session(sid2, TerminationCause::Revoked)
        .unwrap();
    let t = h
        .core
        .terminate_session_with_stop(sid2, TerminationCause::ClientLost)
        .unwrap();
    assert!(t.emergency_stops.is_empty());
    assert_eq!(
        h.core.vm_status(&Caller::User, vm).unwrap().run_id,
        Some(run)
    );
    // Every live session ends at once (agent access off, UI loss).
    let (_, _) = h.session(&[vm], &ALL_CAPS, 3);
    let (_, _) = h.session(&[vm], &ALL_CAPS, 4);
    let all = h
        .core
        .terminate_all_sessions(TerminationCause::Revoked, false);
    assert_eq!(all.len(), 2);
    assert_eq!(h.core.sessions().live().count(), 0);
}

/// Every agent call is audited, in order, without secrets; screenshot reads
/// and lease renewals included.
#[test]
fn agent_calls_are_audited_in_order() {
    let mut h = Harness::with_config(roomy());
    let vm = h.create_vm("a");
    let run = h.start(vm);
    h.screenshot_geometry(vm, run);
    let (sid, agent) = h.session(&[vm], &ALL_CAPS, 0x5a);
    let before = h.audit_events().len();
    h.core.list_vms(&agent).unwrap();
    h.core.vm_status(&agent, vm).unwrap();
    let r = h.req(vm, Some(run), VmAction::Screenshot);
    let shot = r.operation_id;
    h.core.submit(&agent, r).unwrap();
    h.drain(vm);
    h.core.read_screenshot(&agent, vm, shot).unwrap();
    h.core
        .renew_lease(sid, &SessionToken::from_bytes([0x5a; 32]), 10_000)
        .unwrap();
    assert_eq!(
        code(
            h.core
                .renew_lease(sid, &SessionToken::from_bytes([1; 32]), 10_000)
        ),
        Some(ErrorCode::Unauthenticated)
    );
    h.core
        .revoke_session(sid, TerminationCause::Revoked)
        .unwrap();
    assert_eq!(
        code(h.core.read_screenshot(&agent, vm, shot)),
        Some(ErrorCode::Unauthenticated)
    );
    let ev = h.audit_events();
    let mine: Vec<(AuditAction, AuditOutcome)> =
        ev[before..].iter().map(|e| (e.action, e.outcome)).collect();
    assert_eq!(
        mine,
        vec![
            (AuditAction::VmList, AuditOutcome::Succeeded),
            (AuditAction::VmStatus, AuditOutcome::Succeeded),
            (AuditAction::VmScreenshot, AuditOutcome::Accepted),
            (AuditAction::VmScreenshot, AuditOutcome::Forwarded),
            (AuditAction::VmScreenshot, AuditOutcome::Succeeded),
            (AuditAction::ScreenshotRead, AuditOutcome::Succeeded),
            (AuditAction::SessionLease, AuditOutcome::Succeeded),
            (AuditAction::SessionLease, AuditOutcome::Denied),
            // Revocation: the input goes back to the user, then the session ends.
            (AuditAction::InputControl, AuditOutcome::Succeeded),
            (AuditAction::SessionTerminate, AuditOutcome::Succeeded),
            (AuditAction::ScreenshotRead, AuditOutcome::Denied),
        ]
    );
    let seqs: Vec<u64> = ev.iter().map(|e| e.seq).collect();
    assert!(seqs.windows(2).all(|w| w[0] < w[1]));
    let json = serde_json::to_string(&ev).unwrap();
    assert!(!json.contains(&"5a".repeat(32)), "token in the audit");
    assert!(ev[before..].iter().all(|e| e.session_id == Some(sid)));
}

/// Authorized agent × action × VM state: the state guard of the core
/// applies to agents as to the user (no shortcut through a session).
#[test]
fn authorized_agent_calls_follow_the_state_guards() {
    for act in ACTS {
        for running in [false, true] {
            let mut h = Harness::with_config(roomy());
            let vm = h.create_vm("a");
            // A run that existed: stale once the VM is stopped again.
            let run = h.start(vm);
            h.screenshot_geometry(vm, run);
            if !running {
                h.stop(vm);
            }
            let (_, agent) = h.session(&[vm], &ALL_CAPS, 1);
            let a = match act {
                Act::Start => VmAction::Start,
                Act::Stop => VmAction::Stop,
                Act::ForceStop => VmAction::ForceStop,
                Act::Reset => VmAction::Reset,
                Act::Screenshot => VmAction::Screenshot,
                Act::Input => h.input(vm, run, 1),
            };
            let needs_run = matches!(act, Act::Reset | Act::Screenshot | Act::Input);
            let r = h.req(vm, needs_run.then_some(run), a);
            let got = code(h.core.submit(&agent, r));
            let allowed = match act {
                Act::Start => !running,
                _ => running,
            };
            let want = (!allowed).then_some(ErrorCode::StateConflict);
            assert_eq!(got, want, "{act:?} running={running}");
        }
    }
}

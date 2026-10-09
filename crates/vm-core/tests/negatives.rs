//! Percorsi negativi del core con porte finte (nessun QEMU reale): stato non
//! ammesso, VM occupata, quote, sessioni revocate o scadute, distinzione tra
//! `unauthenticated` e `permission_denied`, media non approvati, risorse oltre
//! limite, audit minimo (F1-10). Ogni diniego deve essere senza effetto sul
//! backend e registrato nell'audit con il codice pubblico.

mod common;

use common::*;
use vm_core::proto::*;
use vm_core::session::{SessionSpec, UNAUTHENTICATED_MESSAGE};

fn denied(code: ErrorCode, r: Result<impl std::fmt::Debug, PublicError>) -> PublicError {
    match r {
        Err(e) => {
            assert_eq!(e.code, code, "{e:?}");
            e
        }
        Ok(v) => panic!("atteso {code}, ottenuto {v:?}"),
    }
}

/// Ultimo evento di audit: deve essere un diniego con il codice dato.
fn last_denial(h: &Harness, code: ErrorCode) -> AuditEvent {
    let ev = h.audit_events().pop().expect("audit vuoto");
    assert_eq!(ev.outcome, AuditOutcome::Denied, "{ev:?}");
    assert_eq!(ev.error, Some(code), "{ev:?}");
    ev
}

fn fail_vm(h: &mut Harness, vm: VmId, run: RunId) {
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
    assert_eq!(
        h.core.vm_status(&Caller::User, vm).unwrap().state,
        VmState::Failed
    );
}

// ----- stato non ammesso ----------------------------------------------------

#[test]
fn failed_vm_rejects_every_mutation_until_reconciled() {
    let mut h = Harness::new();
    let vm = h.create_vm("a");
    let run = h.start(vm);
    h.screenshot_geometry(vm, run);
    fail_vm(&mut h, vm, run);
    let calls = h.backend.calls.len();

    for action in [VmAction::Start, VmAction::Stop, VmAction::ForceStop] {
        let r = h.req(vm, None, action);
        let e = denied(ErrorCode::StateConflict, h.core.submit(&Caller::User, r));
        assert_eq!(
            e.details.unwrap().get("state").map(String::as_str),
            Some("failed")
        );
        last_denial(&h, ErrorCode::StateConflict);
    }
    // Le azioni legate all'esecuzione si riferiscono a un run_id chiuso.
    for action in [VmAction::Reset, VmAction::Screenshot, h.input(vm, run, 1)] {
        let r = h.req(vm, Some(run), action);
        denied(ErrorCode::StateConflict, h.core.submit(&Caller::User, r));
    }
    assert_eq!(h.backend.calls.len(), calls, "nessun effetto");
    // `failed → stopped` non è diretto: serve la riconciliazione.
    denied(ErrorCode::StateConflict, h.core.finish_recovery(vm));
    h.core.begin_recovery(vm).unwrap();
    h.core.begin_recovery(vm).unwrap(); // idempotente
    let r = h.req(vm, None, VmAction::Start);
    denied(ErrorCode::StateConflict, h.core.submit(&Caller::User, r));
    h.core.finish_recovery(vm).unwrap();
    let run2 = h.start(vm);
    assert_ne!(run, run2);
}

#[test]
fn starting_vm_accepts_only_force_stop() {
    let mut h = Harness::new();
    let vm = h.create_vm("a");
    let r = h.req(vm, None, VmAction::Start);
    h.core.submit(&Caller::User, r).unwrap();
    let d = h.core.begin_dispatch(vm).unwrap().unwrap();
    assert_eq!(
        h.core.vm_status(&Caller::User, vm).unwrap().state,
        VmState::Starting
    );
    for action in [VmAction::Start, VmAction::Stop] {
        let r = h.req(vm, None, action);
        denied(ErrorCode::StateConflict, h.core.submit(&Caller::User, r));
    }
    let r = h.req(vm, Some(d.run_id), VmAction::Screenshot);
    denied(ErrorCode::StateConflict, h.core.submit(&Caller::User, r));
    // Force-stop ammesso e inoltrato anche con l'avvio ancora in corso.
    let r = h.req(vm, None, VmAction::ForceStop);
    h.core.submit(&Caller::User, r).unwrap();
    let f = h.core.begin_dispatch(vm).unwrap().unwrap();
    assert_eq!(f.action, VmAction::ForceStop);
    assert_eq!(f.run_id, d.run_id);
    assert_eq!(
        h.core.vm_status(&Caller::User, vm).unwrap().state,
        VmState::Stopping
    );
}

// ----- VM occupata -----------------------------------------------------------

#[test]
fn busy_vm_rejects_concurrent_lifecycle_mutations() {
    let mut h = Harness::new();
    let vm = h.create_vm("a");
    let run = h.start(vm);
    let (_sid, agent) = h.session(&[vm], &ALL_CAPS, 1);
    let r = h.req(vm, None, VmAction::Stop);
    h.core.submit(&Caller::User, r).unwrap();
    // Reset e stop dell'agente con uno stop già in coda: vm_busy, ritentabile.
    let r = h.req(vm, Some(run), VmAction::Reset);
    let e = denied(ErrorCode::VmBusy, h.core.submit(&agent, r));
    assert!(e.retryable);
    let ev = last_denial(&h, ErrorCode::VmBusy);
    assert_eq!(ev.capability, Some(Capability::VmReset));
    let r = h.req(vm, None, VmAction::Stop);
    denied(ErrorCode::VmBusy, h.core.submit(&agent, r));
    // Con una mutazione del ciclo di vita pendente anche screenshot e input attendono.
    let r = h.req(vm, Some(run), VmAction::Screenshot);
    denied(ErrorCode::VmBusy, h.core.submit(&agent, r));
    // Un solo arresto forzato alla volta.
    let r = h.req(vm, None, VmAction::ForceStop);
    h.core.submit(&Caller::User, r).unwrap();
    let r = h.req(vm, None, VmAction::ForceStop);
    let e = denied(ErrorCode::VmBusy, h.core.submit(&Caller::User, r));
    assert!(e.retryable);
    assert!(
        h.backend
            .calls
            .iter()
            .all(|c| !matches!(c, Call::Reset(..)))
    );
}

#[test]
fn hard_queue_limit_applies_to_the_user_too() {
    let mut h = Harness::new();
    let vm = h.create_vm("a");
    let run = h.start(vm);
    for _ in 0..vm_core::MAX_QUEUE_LEN {
        let r = h.req(vm, Some(run), VmAction::Screenshot);
        h.core.submit(&Caller::User, r).unwrap();
    }
    let r = h.req(vm, Some(run), VmAction::Screenshot);
    denied(ErrorCode::ResourceLimit, h.core.submit(&Caller::User, r));
    last_denial(&h, ErrorCode::ResourceLimit);
}

// ----- quote -----------------------------------------------------------------

#[test]
fn quota_denials_are_audited_and_global_quotas_bound_the_grant() {
    let mut cfg = config();
    cfg.global_policy.quotas.max_operations = 1;
    let mut h = Harness::with_config(cfg);
    let vm = h.create_vm("a");
    let run = h.start(vm);
    // Il grant chiede quote illimitate, la policy globale ne ammette una.
    let (sid, agent) = h.session(&[vm], &ALL_CAPS, 1);
    let r = h.req(vm, Some(run), VmAction::Screenshot);
    h.core.submit(&agent, r).unwrap();
    let r = h.req(vm, Some(run), VmAction::Screenshot);
    let op = r.operation_id;
    denied(ErrorCode::ResourceLimit, h.core.submit(&agent, r));
    let ev = last_denial(&h, ErrorCode::ResourceLimit);
    assert_eq!(ev.session_id, Some(sid));
    assert_eq!(ev.operation_id, Some(op));
    assert_eq!(ev.actor, Actor::Agent { session_id: sid });
    // L'operazione negata non esiste per il core.
    denied(ErrorCode::NotFound, h.core.operation(&Caller::User, op));
    assert_eq!(h.core.sessions().get(sid).unwrap().usage.operations, 1);
}

#[test]
fn screenshot_over_byte_quota_fails_without_updating_geometry() {
    let mut h = Harness::new();
    let vm = h.create_vm("a");
    let run = h.start(vm);
    let spec = SessionSpec {
        vm_allowlist: [vm].into_iter().collect(),
        capabilities: ALL_CAPS.into_iter().collect(),
        quotas: Quotas {
            max_screenshot_bytes: 999,
            ..Quotas::UNBOUNDED
        },
        expires_at: UtcTimestamp::from_unix_millis(T0 + 60_000).unwrap(),
        lease_millis: 10_000,
    };
    let (grant, _) = h
        .core
        .create_session(spec, SessionToken::from_bytes([4; 32]))
        .unwrap();
    let agent = Caller::Agent {
        session_id: grant.session_id,
        token: Some(SessionToken::from_bytes([4; 32])),
    };
    let r = h.req(vm, Some(run), VmAction::Screenshot);
    h.core.submit(&agent, r).unwrap();
    let rec = h.run_next(vm).unwrap();
    // Il backend finto produce 1000 byte: oltre il limite di 999.
    assert_eq!(rec.status, OperationStatus::Failed);
    assert_eq!(rec.error, Some(ErrorCode::ResourceLimit));
    assert_eq!(h.core.vm_status(&Caller::User, vm).unwrap().geometry, None);
}

// ----- sessioni: unauthenticated, session_expired, permission_denied --------

#[test]
fn unauthenticated_and_permission_denied_are_distinct() {
    let mut h = Harness::new();
    let a = h.create_vm("a");
    let b = h.create_vm("b");
    let run = h.start(a);
    let (sid, agent) = h.session(&[a], &[Capability::VmStatus, Capability::VmList], 1);
    let wrong = Caller::Agent {
        session_id: sid,
        token: Some(SessionToken::from_bytes([9; 32])),
    };

    // Credenziale non valida: unauthenticated, anche su una VM fuori allowlist
    // (l'autenticazione precede ogni altra verifica e non rivela nulla su B).
    let e = denied(ErrorCode::Unauthenticated, h.core.vm_status(&wrong, a));
    assert_eq!(e.message, UNAUTHENTICATED_MESSAGE);
    assert!(!e.retryable);
    let e2 = denied(ErrorCode::Unauthenticated, h.core.vm_status(&wrong, b));
    assert_eq!(e, e2, "stessa risposta, nessuna informazione su B");
    denied(ErrorCode::Unauthenticated, h.core.list_vms(&wrong));
    let ev = last_denial(&h, ErrorCode::Unauthenticated);
    assert_eq!(ev.actor, Actor::Agent { session_id: sid });

    // Credenziale valida, autorizzazione insufficiente: permission_denied.
    denied(ErrorCode::PermissionDenied, h.core.vm_status(&agent, b));
    let r = h.req(a, Some(run), VmAction::Screenshot);
    let e = denied(ErrorCode::PermissionDenied, h.core.submit(&agent, r));
    assert_eq!(
        e.details.unwrap().get("capability").map(String::as_str),
        Some("vm.screenshot")
    );
    let cfg = h.vm_config("x", None);
    denied(ErrorCode::PermissionDenied, h.core.create_vm(&agent, cfg));
    last_denial(&h, ErrorCode::PermissionDenied);

    // Le stesse richieste con credenziale assente: unauthenticated.
    let none = Caller::Agent {
        session_id: sid,
        token: None,
    };
    let r = h.req(a, Some(run), VmAction::Screenshot);
    denied(ErrorCode::Unauthenticated, h.core.submit(&none, r));
    assert_eq!(h.backend.count(|c| matches!(c, Call::Screenshot(..))), 0);
}

#[test]
fn revoked_session_is_unauthenticated_everywhere_and_queue_is_discarded() {
    let mut h = Harness::new();
    let vm = h.create_vm("a");
    let run = h.start(vm);
    let (sid, agent) = h.session(&[vm], &ALL_CAPS, 1);
    let r = h.req(vm, Some(run), VmAction::Screenshot);
    let mine = r.operation_id;
    h.core.submit(&agent, r).unwrap();
    let t = h
        .core
        .revoke_session(sid, TerminationCause::Revoked)
        .unwrap();
    assert_eq!(t.report.discarded, vec![mine]);
    let op = h.core.operation(&Caller::User, mine).unwrap();
    assert_eq!(op.status, OperationStatus::Discarded);
    assert_eq!(op.error, Some(ErrorCode::Unauthenticated));

    denied(ErrorCode::Unauthenticated, h.core.vm_status(&agent, vm));
    denied(ErrorCode::Unauthenticated, h.core.list_vms(&agent));
    denied(ErrorCode::Unauthenticated, h.core.operation(&agent, mine));
    let r = h.req(vm, None, VmAction::Stop);
    denied(ErrorCode::Unauthenticated, h.core.submit(&agent, r));
    last_denial(&h, ErrorCode::Unauthenticated);
    // Una revoca ripetuta non produce una seconda terminazione.
    let again = h
        .core
        .revoke_session(sid, TerminationCause::Revoked)
        .unwrap();
    assert!(again.report.discarded.is_empty());
    // La ripresa non riattiva una sessione revocata.
    denied(ErrorCode::StateConflict, h.core.resume_session(sid));
    denied(
        ErrorCode::NotFound,
        h.core.revoke_session(
            SessionId::from_unix_millis(5, [5; 10]),
            TerminationCause::Revoked,
        ),
    );
    assert_eq!(h.backend.count(|c| matches!(c, Call::Screenshot(..))), 0);
}

#[test]
fn every_non_expiry_termination_cause_yields_unauthenticated() {
    for cause in [
        TerminationCause::Completed,
        TerminationCause::Revoked,
        TerminationCause::EmergencyStop,
        TerminationCause::ClientLost,
        TerminationCause::UiClosed,
        TerminationCause::SupervisorLost,
        TerminationCause::HostResumed,
        TerminationCause::VmTerminated,
        TerminationCause::Restored,
    ] {
        let mut h = Harness::new();
        let vm = h.create_vm("a");
        let (sid, agent) = h.session(&[vm], &ALL_CAPS, 1);
        h.core.revoke_session(sid, cause).unwrap();
        denied(ErrorCode::Unauthenticated, h.core.vm_status(&agent, vm));
        assert_eq!(
            vm_core::session::discard_code_for(cause),
            ErrorCode::Unauthenticated
        );
    }
    for cause in [
        TerminationCause::GrantExpired,
        TerminationCause::LeaseExpired,
    ] {
        let mut h = Harness::new();
        let vm = h.create_vm("a");
        let (sid, agent) = h.session(&[vm], &ALL_CAPS, 1);
        h.core.revoke_session(sid, cause).unwrap();
        denied(ErrorCode::SessionExpired, h.core.vm_status(&agent, vm));
        assert_eq!(
            vm_core::session::discard_code_for(cause),
            ErrorCode::SessionExpired
        );
    }
}

#[test]
fn expired_session_is_session_expired_before_and_after_tick() {
    let mut h = Harness::new();
    let vm = h.create_vm("a");
    let run = h.start(vm);
    let (sid, agent) = h.session(&[vm], &ALL_CAPS, 1);
    let r = h.req(vm, Some(run), VmAction::Screenshot);
    let queued = r.operation_id;
    h.core.submit(&agent, r).unwrap();
    // Pausa e lease scaduto insieme: prevale la scadenza.
    h.core.pause_session(sid).unwrap();
    h.clock.advance(10_000);
    let r = h.req(vm, None, VmAction::Stop);
    denied(ErrorCode::SessionExpired, h.core.submit(&agent, r));
    last_denial(&h, ErrorCode::SessionExpired);
    denied(ErrorCode::SessionExpired, h.core.vm_status(&agent, vm));
    denied(ErrorCode::SessionExpired, h.core.list_vms(&agent));
    denied(ErrorCode::SessionExpired, h.core.operation(&agent, queued));
    denied(
        ErrorCode::SessionExpired,
        h.core
            .renew_lease(sid, &SessionToken::from_bytes([1; 32]), 10_000),
    );
    denied(ErrorCode::SessionExpired, h.core.resume_session(sid));
    // Il controllo periodico termina la sessione e scarta la coda con lo stesso codice.
    let terms = h.core.tick().unwrap();
    assert_eq!(terms.len(), 1);
    assert_eq!(terms[0].cause, TerminationCause::LeaseExpired);
    assert_eq!(terms[0].report.discarded, vec![queued]);
    let op = h.core.operation(&Caller::User, queued).unwrap();
    assert_eq!(op.error, Some(ErrorCode::SessionExpired));
    denied(ErrorCode::SessionExpired, h.core.vm_status(&agent, vm));
    // Un token errato su una sessione scaduta resta unauthenticated.
    let wrong = Caller::Agent {
        session_id: sid,
        token: Some(SessionToken::from_bytes([2; 32])),
    };
    denied(ErrorCode::Unauthenticated, h.core.vm_status(&wrong, vm));
    assert_eq!(h.backend.count(|c| matches!(c, Call::Screenshot(..))), 0);
}

#[test]
fn renewal_cannot_extend_beyond_grant_or_lease_limit() {
    let mut h = Harness::new();
    let vm = h.create_vm("a");
    let (sid, _agent) = h.session(&[vm], &ALL_CAPS, 1);
    let token = SessionToken::from_bytes([1; 32]);
    // Durata richiesta oltre 10 s: ridotta al massimo.
    let lease = h.core.renew_lease(sid, &token, 60_000).unwrap();
    assert_eq!(
        lease.expires_at.millis_since(lease.renewed_at),
        LEASE_MAX_MILLIS
    );
    denied(
        ErrorCode::InvalidArgument,
        h.core.renew_lease(sid, &token, 0),
    );
    denied(
        ErrorCode::Unauthenticated,
        h.core
            .renew_lease(sid, &SessionToken::from_bytes([3; 32]), 10_000),
    );
    // Rinnovi continui non superano la scadenza del grant (1 h).
    for _ in 0..500 {
        h.clock.advance(9_000);
        if h.core.renew_lease(sid, &token, 10_000).is_err() {
            break;
        }
    }
    denied(
        ErrorCode::SessionExpired,
        h.core.renew_lease(sid, &token, 10_000),
    );
    let terms = h.core.tick().unwrap();
    assert_eq!(terms[0].cause, TerminationCause::GrantExpired);
}

// ----- media non approvati ----------------------------------------------------

fn start_with_media_fault(fault: impl FnOnce(&mut Harness, MediaId)) -> (Harness, OperationRecord) {
    let mut h = Harness::new();
    let m = h.install_media();
    let cfg = h.vm_config("a", Some(m));
    let vm = h.core.create_vm(&Caller::User, cfg).unwrap().id;
    fault(&mut h, m);
    let r = h.req(vm, None, VmAction::Start);
    let op = r.operation_id;
    h.core.submit(&Caller::User, r).unwrap();
    // Un'operazione scartata prima dell'inoltro non produce esito dal backend.
    let _ = h.run_next(vm);
    let rec = h.core.operation(&Caller::User, op).unwrap();
    assert_eq!(h.backend.count(|c| matches!(c, Call::Start(..))), 0);
    (h, rec)
}

#[test]
fn unapproved_media_never_reaches_the_backend() {
    for (name, fault) in [
        ("writable", 0u8),
        ("moved", 1),
        ("replaced", 2),
        ("unreadable", 3),
    ] {
        let (h, rec) = start_with_media_fault(|h, m| {
            match fault {
                0 => h.inspector.writable.insert(m),
                1 => h.inspector.moved.insert(m),
                2 => h.inspector.replaced.insert(m),
                _ => h.inspector.unreadable.insert(m),
            };
        });
        assert_eq!(rec.status, OperationStatus::Failed, "{name}");
        let expected = if fault == 3 {
            ErrorCode::InfrastructureError
        } else {
            ErrorCode::PermissionDenied
        };
        assert_eq!(rec.error, Some(expected), "{name}");
        let failed = h
            .audit_events()
            .into_iter()
            .rfind(|e| e.action == AuditAction::VmStart)
            .unwrap();
        assert_eq!(failed.outcome, AuditOutcome::Failed, "{name}");
        assert_eq!(failed.error, Some(expected), "{name}");
        let run = h.store.0.borrow().runs.values().next().cloned().unwrap();
        assert_eq!(run.end_reason, Some(RunEndReason::StartFailed), "{name}");
    }
}

#[test]
fn media_removed_from_registry_discards_start() {
    let (h, rec) = start_with_media_fault(|h, m| {
        h.store.0.borrow_mut().media.remove(&m);
    });
    assert_eq!(rec.status, OperationStatus::Discarded);
    assert_eq!(rec.error, Some(ErrorCode::NotFound));
    assert!(
        h.store.0.borrow().runs.is_empty(),
        "nessuna esecuzione creata"
    );
    let vm = h.store.0.borrow().vms.keys().next().copied().unwrap();
    assert_eq!(
        h.store.0.borrow().vms[&vm].persisted_state,
        VmState::Stopped
    );
}

#[test]
fn empty_media_cannot_be_registered() {
    let mut h = Harness::new();
    let e = h
        .core
        .register_media(vm_core::MediaRegistration {
            kind: MediaKind::InstallIso,
            sha256: Sha256Digest::from_bytes([1; 32]),
            size_bytes: 0,
            declared_origin: Label::parse("vuota").unwrap(),
            managed_path: ManagedRelPath::parse("media/vuota.iso").unwrap(),
            file_identity: None,
        })
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::InvalidArgument);
    assert!(h.store.0.borrow().media.is_empty());
}

// ----- risorse oltre limite ---------------------------------------------------

#[test]
fn total_guest_memory_is_bounded_by_host_memory_minus_reserve() {
    let mut cfg = config();
    cfg.global_limits.max_running_vms = 3;
    let mut h = Harness::with_config(cfg);
    // Host: 32 GiB totali, 4 GiB di riserva → 28 GiB per i guest.
    let m = h.install_media();
    let mut ids = Vec::new();
    for name in ["a", "b"] {
        let mut c = h.vm_config(name, Some(m));
        c.memory_mib = 16 * 1024;
        ids.push(h.core.create_vm(&Caller::User, c).unwrap().id);
    }
    let mut small = h.vm_config("c", Some(m));
    small.memory_mib = 8 * 1024;
    let c = h.core.create_vm(&Caller::User, small).unwrap().id;
    h.start(ids[0]);
    let r = h.req(ids[1], None, VmAction::Start);
    let e = denied(ErrorCode::ResourceLimit, h.core.submit(&Caller::User, r));
    let d = e.details.unwrap();
    assert_eq!(d.get("field").map(String::as_str), Some("memory_mib_total"));
    assert_eq!(d.get("requested").map(String::as_str), Some("32768"));
    assert_eq!(d.get("limit").map(String::as_str), Some("28672"));
    last_denial(&h, ErrorCode::ResourceLimit);
    // 16 + 8 GiB rientrano.
    h.start(c);
    assert_eq!(h.backend.count(|x| matches!(x, Call::Start(..))), 2);
}

#[test]
fn host_capacity_change_is_rechecked_at_start_and_before_forwarding() {
    let mut h = Harness::new();
    let vm = h.create_vm("a");
    // Riserva aumentata dopo la creazione: la VM da 4 GiB non entra più.
    let mut host = host();
    host.reserved_memory_mib = 30 * 1024;
    h.core.set_host_capacity(host);
    let r = h.req(vm, None, VmAction::Start);
    let e = denied(ErrorCode::ResourceLimit, h.core.submit(&Caller::User, r));
    assert_eq!(
        e.details.unwrap().get("field").map(String::as_str),
        Some("memory_mib")
    );
    // Accettata con la capacità originale, poi la capacità cala prima dell'inoltro.
    h.core.set_host_capacity(common::host());
    let r = h.req(vm, None, VmAction::Start);
    let op = r.operation_id;
    h.core.submit(&Caller::User, r).unwrap();
    let mut host = common::host();
    host.logical_cpus = 1;
    h.core.set_host_capacity(host);
    assert!(h.run_next(vm).is_none());
    let rec = h.core.operation(&Caller::User, op).unwrap();
    assert_eq!(rec.status, OperationStatus::Discarded);
    assert_eq!(rec.error, Some(ErrorCode::ResourceLimit));
    assert!(h.backend.calls.is_empty());
    // Senza acceleratore l'avvio non è disponibile (nessun ripiego TCG).
    let mut host = common::host();
    host.accelerator_available = false;
    h.core.set_host_capacity(host);
    let r = h.req(vm, None, VmAction::Start);
    denied(ErrorCode::Unsupported, h.core.submit(&Caller::User, r));
}

#[test]
fn user_limits_and_host_limits_are_intersected() {
    // Limite utente più stretto dell'host.
    let mut cfg = config();
    cfg.global_limits.max_vcpus = 2;
    let mut h = Harness::with_config(cfg);
    let m = h.install_media();
    let mut c = h.vm_config("a", Some(m));
    c.vcpus = 3;
    let e = denied(ErrorCode::ResourceLimit, h.core.create_vm(&Caller::User, c));
    assert_eq!(
        e.details.unwrap().get("limit").map(String::as_str),
        Some("2")
    );
    last_denial(&h, ErrorCode::ResourceLimit);
    // Host più stretto del limite utente.
    let mut cfg = config();
    cfg.global_limits.max_vcpus = 64;
    cfg.host.logical_cpus = 4;
    let mut h = Harness::with_config(cfg);
    let m = h.install_media();
    let mut c = h.vm_config("a", Some(m));
    c.vcpus = 5;
    let e = denied(ErrorCode::ResourceLimit, h.core.create_vm(&Caller::User, c));
    assert_eq!(
        e.details.unwrap().get("limit").map(String::as_str),
        Some("4")
    );
    // Minimi.
    let mut c = h.vm_config("a", Some(m));
    c.memory_mib = vm_core::resources::MIN_MEMORY_MIB - 1;
    denied(
        ErrorCode::InvalidArgument,
        h.core.create_vm(&Caller::User, c),
    );
    let mut c = h.vm_config("a", Some(m));
    c.disk_gib = vm_core::resources::MIN_DISK_GIB - 1;
    denied(
        ErrorCode::InvalidArgument,
        h.core.create_vm(&Caller::User, c),
    );
    assert!(h.store.0.borrow().vms.is_empty(), "nessuna VM creata");
}

#[test]
fn agent_session_limit_and_rejected_sessions_are_audited() {
    let mut h = Harness::new();
    let vm = h.create_vm("a");
    h.session(&[vm], &ALL_CAPS, 1);
    let spec = SessionSpec {
        vm_allowlist: [vm].into_iter().collect(),
        capabilities: ALL_CAPS.into_iter().collect(),
        quotas: Quotas::UNBOUNDED,
        expires_at: UtcTimestamp::from_unix_millis(T0 + 60_000).unwrap(),
        lease_millis: 10_000,
    };
    denied(
        ErrorCode::ResourceLimit,
        h.core
            .create_session(spec, SessionToken::from_bytes([2; 32])),
    );
    let ev = last_denial(&h, ErrorCode::ResourceLimit);
    assert_eq!(ev.action, AuditAction::SessionCreate);
    assert_eq!(ev.actor, Actor::User);
}

// ----- audit minimo (F1-10) -----------------------------------------------------

#[test]
fn minimal_audit_covers_create_start_stop_and_denials() {
    let mut h = Harness::new();
    let vm = h.create_vm("a");
    let mut bad = h.vm_config("b", None);
    bad.vcpus = 0;
    let _ = h.core.create_vm(&Caller::User, bad);
    let run = h.start(vm);
    let (sid, agent) = h.session(&[vm], &[Capability::VmStatus], 1);
    let r = h.req(vm, Some(run), VmAction::Screenshot);
    let _ = h.core.submit(&agent, r); // permission_denied
    let ghost = Caller::Agent {
        session_id: sid,
        token: None,
    };
    let r = h.req(vm, None, VmAction::Stop);
    let _ = h.core.submit(&ghost, r); // unauthenticated
    let r = h.req(vm, None, VmAction::Start);
    let _ = h.core.submit(&Caller::User, r); // state_conflict
    h.stop(vm);

    let ev = h.audit_events();
    let has = |action: AuditAction, outcome: AuditOutcome, error: Option<ErrorCode>| {
        ev.iter()
            .any(|e| e.action == action && e.outcome == outcome && e.error == error)
    };
    assert!(has(AuditAction::VmCreate, AuditOutcome::Succeeded, None));
    assert!(has(
        AuditAction::VmCreate,
        AuditOutcome::Denied,
        Some(ErrorCode::InvalidArgument)
    ));
    for outcome in [
        AuditOutcome::Accepted,
        AuditOutcome::Forwarded,
        AuditOutcome::Succeeded,
    ] {
        assert!(
            has(AuditAction::VmStart, outcome, None),
            "start {outcome:?}"
        );
        assert!(has(AuditAction::VmStop, outcome, None), "stop {outcome:?}");
    }
    assert!(has(
        AuditAction::VmScreenshot,
        AuditOutcome::Denied,
        Some(ErrorCode::PermissionDenied)
    ));
    assert!(has(
        AuditAction::VmStop,
        AuditOutcome::Denied,
        Some(ErrorCode::Unauthenticated)
    ));
    assert!(has(
        AuditAction::VmStart,
        AuditOutcome::Denied,
        Some(ErrorCode::StateConflict)
    ));
    // Ogni diniego di una richiesta su VM porta VM, operazione e capability.
    for e in ev
        .iter()
        .filter(|e| e.outcome == AuditOutcome::Denied && e.action != AuditAction::VmCreate)
    {
        assert_eq!(e.vm_id, Some(vm), "{e:?}");
        assert!(e.operation_id.is_some(), "{e:?}");
        assert!(e.capability.is_some(), "{e:?}");
    }
    // Ordine totale e istanti presenti.
    assert!(ev.windows(2).all(|w| w[1].seq == w[0].seq + 1));
    assert_eq!(ev[0].seq, 1);
}

#[test]
fn audit_failure_before_effect_blocks_the_operation() {
    let mut h = Harness::new();
    let vm = h.create_vm("a");
    h.audit.1.set(true);
    let r = h.req(vm, None, VmAction::Start);
    let op = r.operation_id;
    let e = h.core.submit(&Caller::User, r).unwrap_err();
    assert_eq!(e.code, ErrorCode::InfrastructureError);
    assert!(h.core.audit_failures() >= 1);
    assert!(h.run_next(vm).is_none(), "nulla in coda");
    denied(ErrorCode::NotFound, h.core.operation(&Caller::User, op));
    assert!(h.backend.calls.is_empty());
    // Ripristinata la destinazione, la stessa richiesta procede.
    h.audit.1.set(false);
    h.start(vm);
}

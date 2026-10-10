//! Scenari del core con porte finte (nessun QEMU reale). Coprono i test
//! negativi della ROADMAP 7.4 realizzabili senza QEMU: Sessioni, VM, Host
//! (parametri), Risorse, Revoca (coda), Lifecycle (scadenza/perdita client), File.

mod common;

use common::*;
use vm_core::proto::*;
use vm_core::{DispatchOutcome, action_allowed_in};

fn denied(code: ErrorCode, r: Result<impl std::fmt::Debug, PublicError>) {
    match r {
        Err(e) => assert_eq!(e.code, code, "{e:?}"),
        Ok(v) => panic!("atteso {code}, ottenuto {v:?}"),
    }
}

// ----- catalogo, risorse, media -------------------------------------------

#[test]
fn create_vm_validates_resources_and_media() {
    let mut h = Harness::new();
    let m = h.install_media();
    let mut cfg = h.vm_config("big", Some(m));
    cfg.memory_mib = 64 * 1024;
    denied(
        ErrorCode::ResourceLimit,
        h.core.create_vm(&Caller::User, cfg.clone()),
    );
    cfg.memory_mib = 4096;
    cfg.vcpus = 64;
    denied(
        ErrorCode::ResourceLimit,
        h.core.create_vm(&Caller::User, cfg.clone()),
    );
    cfg.vcpus = 0;
    denied(
        ErrorCode::InvalidArgument,
        h.core.create_vm(&Caller::User, cfg.clone()),
    );
    cfg.vcpus = 2;
    cfg.disk_gib = 10_000;
    denied(
        ErrorCode::ResourceLimit,
        h.core.create_vm(&Caller::User, cfg.clone()),
    );
    cfg.disk_gib = 40;
    // Media inesistente o di tipo sbagliato.
    let mut bad = cfg.clone();
    bad.install_media = Some(MediaId::from_unix_millis(1, [0x42; 10]));
    denied(ErrorCode::NotFound, h.core.create_vm(&Caller::User, bad));
    let data = h.media(MediaKind::DataIso, "data.iso");
    let mut bad = cfg.clone();
    bad.install_media = Some(data);
    denied(
        ErrorCode::InvalidArgument,
        h.core.create_vm(&Caller::User, bad),
    );
    let mut bad = cfg.clone();
    bad.data_media = Some(m);
    denied(
        ErrorCode::InvalidArgument,
        h.core.create_vm(&Caller::User, bad),
    );
    // Configurazione valida con supporto dati.
    cfg.data_media = Some(data);
    let rec = h.core.create_vm(&Caller::User, cfg).unwrap();
    assert_eq!(rec.persisted_state, VmState::Stopped);
    assert!(h.store.0.borrow().vms.contains_key(&rec.id));
    // Il disco virtuale è limitato anche dallo spazio host libero meno la riserva.
    let mut cfg2 = h.vm_config("b2", Some(m));
    cfg2.disk_gib = 451;
    let mut small = config();
    small.global_limits.max_disk_gib = 10_000;
    let mut h2 = Harness::with_config(small);
    let m2 = h2.install_media();
    cfg2.install_media = Some(m2);
    denied(
        ErrorCode::ResourceLimit,
        h2.core.create_vm(&Caller::User, cfg2),
    );
}

#[test]
fn agent_cannot_create_vm_and_denial_is_audited() {
    let mut h = Harness::new();
    let vm = h.create_vm("a");
    let (_sid, agent) = h.session(&[vm], &ALL_CAPS, 1);
    let cfg = h.vm_config("x", None);
    denied(ErrorCode::PermissionDenied, h.core.create_vm(&agent, cfg));
    let last = h.audit_events().pop().unwrap();
    assert_eq!(last.action, AuditAction::VmCreate);
    assert_eq!(last.outcome, AuditOutcome::Denied);
    assert_eq!(last.error, Some(ErrorCode::PermissionDenied));
}

#[test]
fn replaced_media_is_not_used() {
    let mut h = Harness::new();
    let m = h.install_media();
    let cfg = h.vm_config("a", Some(m));
    let vm = h.core.create_vm(&Caller::User, cfg).unwrap().id;
    h.inspector.replaced.insert(m);
    let r = h.req(vm, None, VmAction::Start);
    h.core.submit(&Caller::User, r).unwrap();
    let rec = h.run_next(vm).unwrap();
    assert_eq!(rec.status, OperationStatus::Failed);
    assert_eq!(rec.error, Some(ErrorCode::PermissionDenied));
    assert_eq!(
        h.backend.count(|c| matches!(c, Call::Start(..))),
        0,
        "QEMU non avviato"
    );
    assert_eq!(
        h.core.vm_status(&Caller::User, vm).unwrap().state,
        VmState::Failed
    );
}

#[test]
fn verify_media_checks_every_field() {
    use vm_core::media::{ObservedMediaFacts, verify_media};
    let entry = MediaEntry {
        record: MediaRecord {
            id: MediaId::from_unix_millis(1, [1; 10]),
            kind: MediaKind::InstallIso,
            sha256: Sha256Digest::from_bytes([1; 32]),
            size_bytes: 10,
            declared_origin: Label::parse("x").unwrap(),
            approved_at: UtcTimestamp::from_unix_millis(0).unwrap(),
        },
        managed_path: ManagedRelPath::parse("media/x.iso").unwrap(),
        file_identity: Some(FileIdentity {
            volume: 1,
            file_id: [2; 16],
        }),
    };
    let ok = ObservedMediaFacts {
        sha256: entry.record.sha256,
        size_bytes: 10,
        file_identity: entry.file_identity,
        read_only: true,
    };
    let now = UtcTimestamp::from_unix_millis(5).unwrap();
    assert!(verify_media(&entry, &ok, now).is_ok());
    for bad in [
        ObservedMediaFacts {
            size_bytes: 11,
            ..ok.clone()
        },
        ObservedMediaFacts {
            sha256: Sha256Digest::from_bytes([3; 32]),
            ..ok.clone()
        },
        ObservedMediaFacts {
            file_identity: Some(FileIdentity {
                volume: 1,
                file_id: [4; 16],
            }),
            ..ok.clone()
        },
        ObservedMediaFacts {
            file_identity: None,
            ..ok.clone()
        },
        ObservedMediaFacts {
            read_only: false,
            ..ok.clone()
        },
    ] {
        let e = verify_media(&entry, &bad, now).unwrap_err();
        assert_eq!(e.code, ErrorCode::PermissionDenied);
    }
}

// ----- ciclo di vita, coda, idempotenza ------------------------------------

#[test]
fn start_stop_cycles_use_fresh_run_ids_and_audit_in_order() {
    let mut h = Harness::new();
    let vm = h.create_vm("cicli");
    let mut runs = std::collections::BTreeSet::new();
    for _ in 0..20 {
        let run = h.start(vm);
        assert!(runs.insert(run), "run_id ripetuto");
        h.stop(vm);
        let st = h.core.vm_status(&Caller::User, vm).unwrap();
        assert_eq!(st.state, VmState::Stopped);
        assert_eq!(st.run_id, None);
    }
    assert_eq!(h.backend.count(|c| matches!(c, Call::Start(..))), 20);
    assert_eq!(h.backend.count(|c| matches!(c, Call::Shutdown(..))), 20);
    let store = h.store.0.borrow();
    assert_eq!(store.runs.len(), 20);
    assert!(
        store
            .runs
            .values()
            .all(|r| r.end_reason == Some(RunEndReason::Stopped))
    );
    drop(store);
    let ev = h.audit_events();
    assert!(
        ev.windows(2).all(|w| w[1].seq == w[0].seq + 1),
        "ordine totale"
    );
    assert_eq!(ev[0].action, AuditAction::VmCreate);
    let starts = ev
        .iter()
        .filter(|e| e.action == AuditAction::VmStart && e.outcome == AuditOutcome::Succeeded)
        .count();
    let stops = ev
        .iter()
        .filter(|e| e.action == AuditAction::VmStop && e.outcome == AuditOutcome::Succeeded)
        .count();
    assert_eq!((starts, stops), (20, 20));
}

#[test]
fn incompatible_operations_are_rejected() {
    let mut h = Harness::new();
    let vm = h.create_vm("a");
    let r = h.req(vm, None, VmAction::Stop);
    denied(ErrorCode::StateConflict, h.core.submit(&Caller::User, r));
    let r = h.req(vm, None, VmAction::Start);
    h.core.submit(&Caller::User, r).unwrap();
    let r = h.req(vm, None, VmAction::Start);
    denied(ErrorCode::VmBusy, h.core.submit(&Caller::User, r));
    let r = h.req(vm, None, VmAction::Start);
    let busy = h.core.submit(&Caller::User, r).unwrap_err();
    assert!(busy.retryable, "vm_busy è ritentabile");
    h.drain(vm);
    let r = h.req(vm, None, VmAction::Start);
    denied(ErrorCode::StateConflict, h.core.submit(&Caller::User, r));
    // Stop in corso: nuovo stop → state_conflict (la tabella di stato ammette
    // `stop` solo da `running`; il rimedio è il force-stop), force-stop ammesso.
    let r = h.req(vm, None, VmAction::Stop);
    h.core.submit(&Caller::User, r).unwrap();
    let d = h.core.begin_dispatch(vm).unwrap().unwrap();
    assert_eq!(d.action, VmAction::Stop);
    let r = h.req(vm, None, VmAction::Stop);
    denied(ErrorCode::StateConflict, h.core.submit(&Caller::User, r));
    let r = h.req(vm, None, VmAction::ForceStop);
    h.core.submit(&Caller::User, r).unwrap();
    // L'arresto forzato si inoltra anche con lo stop ancora pendente.
    let f = h.core.begin_dispatch(vm).unwrap().unwrap();
    assert_eq!(f.action, VmAction::ForceStop);
    let rec = h
        .core
        .finish_dispatch(vm, f.operation_id, Ok(DispatchOutcome::Done))
        .unwrap();
    assert_eq!(rec.status, OperationStatus::Succeeded);
    assert_eq!(
        h.core.vm_status(&Caller::User, vm).unwrap().state,
        VmState::Stopped
    );
    // Lo stop pendente si chiude senza cambiare stato.
    h.core
        .finish_dispatch(vm, d.operation_id, Ok(DispatchOutcome::Done))
        .unwrap();
    assert_eq!(
        h.core.vm_status(&Caller::User, vm).unwrap().state,
        VmState::Stopped
    );
    let last_run = h.store.0.borrow().runs.values().next().cloned().unwrap();
    assert_eq!(last_run.end_reason, Some(RunEndReason::ForceStopped));
}

#[test]
fn only_one_operation_in_flight_per_vm() {
    let mut h = Harness::new();
    let vm = h.create_vm("a");
    let run = h.start(vm);
    h.screenshot_geometry(vm, run);
    for _ in 0..3 {
        let a = h.input(vm, run, 1);
        let r = h.req(vm, Some(run), a);
        h.core.submit(&Caller::User, r).unwrap();
    }
    let first = h.core.begin_dispatch(vm).unwrap().unwrap();
    assert!(h.core.begin_dispatch(vm).unwrap().is_none());
    h.core
        .finish_dispatch(vm, first.operation_id, Ok(DispatchOutcome::Done))
        .unwrap();
    assert!(h.core.begin_dispatch(vm).unwrap().is_some());
}

#[test]
fn operation_id_is_idempotent() {
    let mut h = Harness::new();
    let vm = h.create_vm("a");
    let req = h.req(vm, None, VmAction::Start);
    let a1 = h.core.submit(&Caller::User, req.clone()).unwrap();
    assert!(!a1.duplicate);
    let a2 = h.core.submit(&Caller::User, req.clone()).unwrap();
    assert!(a2.duplicate);
    assert_eq!(a2.status, OperationStatus::Queued);
    h.drain(vm);
    let a3 = h.core.submit(&Caller::User, req.clone()).unwrap();
    assert!(a3.duplicate);
    assert_eq!(a3.status, OperationStatus::Succeeded);
    assert_eq!(
        h.backend.count(|c| matches!(c, Call::Start(..))),
        1,
        "un solo effetto"
    );
    // Stesso id con azione diversa: rifiutato.
    let other = VmRequest {
        action: VmAction::Stop,
        ..req
    };
    denied(
        ErrorCode::InvalidArgument,
        h.core.submit(&Caller::User, other),
    );
}

#[test]
fn stale_run_id_is_rejected_and_queued_ops_invalidated() {
    let mut h = Harness::new();
    let vm = h.create_vm("a");
    let run1 = h.start(vm);
    h.screenshot_geometry(vm, run1);
    // Input accodato per run1, poi la VM esce in modo inatteso.
    let a = h.input(vm, run1, 2);
    let r = h.req(vm, Some(run1), a);
    let queued = r.operation_id;
    h.core.submit(&Caller::User, r).unwrap();
    assert!(
        h.core
            .observe(
                vm,
                BackendObservation::ProcessExited {
                    run_id: run1,
                    expected: false
                }
            )
            .unwrap()
    );
    let st = h.core.vm_status(&Caller::User, vm).unwrap();
    assert_eq!(st.state, VmState::Failed);
    let op = h.core.operation(&Caller::User, queued).unwrap();
    assert_eq!(op.status, OperationStatus::Discarded);
    assert_eq!(op.error, Some(ErrorCode::StateConflict));
    // Riconciliazione e nuovo avvio.
    h.core.begin_recovery(vm).unwrap();
    h.core.finish_recovery(vm).unwrap();
    let run2 = h.start(vm);
    assert_ne!(run1, run2);
    let r = h.req(vm, Some(run1), VmAction::Screenshot);
    let e = h.core.submit(&Caller::User, r).unwrap_err();
    assert_eq!(e.code, ErrorCode::StateConflict);
    assert_eq!(
        e.details.unwrap().get("reason").map(String::as_str),
        Some("stale_run_id")
    );
    // Osservazioni della vecchia esecuzione ignorate.
    assert!(
        !h.core
            .observe(
                vm,
                BackendObservation::ProcessExited {
                    run_id: run1,
                    expected: false
                }
            )
            .unwrap()
    );
    assert_eq!(
        h.core.vm_status(&Caller::User, vm).unwrap().state,
        VmState::Running
    );
    assert_eq!(h.backend.count(|c| matches!(c, Call::Input(..))), 0);
}

#[test]
fn input_requires_current_geometry_and_valid_batch() {
    let mut h = Harness::new();
    let vm = h.create_vm("a");
    let run = h.start(vm);
    let a = h.input(vm, run, 1);
    let r = h.req(vm, Some(run), a);
    denied(ErrorCode::StateConflict, h.core.submit(&Caller::User, r));
    h.screenshot_geometry(vm, run);
    let a = h.input(vm, run, 1);
    let r = h.req(vm, Some(run), a);
    h.core.submit(&Caller::User, r).unwrap();
    // Lotto oltre il massimo, vuoto, run mancante, coordinate fuori intervallo.
    let a = h.input(vm, run, MAX_INPUT_EVENTS_PER_REQUEST + 1);
    let r = h.req(vm, Some(run), a);
    denied(ErrorCode::ResourceLimit, h.core.submit(&Caller::User, r));
    let a = h.input(vm, run, 0);
    let r = h.req(vm, Some(run), a);
    denied(ErrorCode::InvalidArgument, h.core.submit(&Caller::User, r));
    let a = h.input(vm, run, 1);
    let r = h.req(vm, None, a);
    denied(ErrorCode::InvalidArgument, h.core.submit(&Caller::User, r));
    let bad = VmAction::Input(InputBatch {
        vm_id: vm,
        run_id: run,
        geometry: DisplayGeometry {
            width: 1280,
            height: 800,
        },
        events: vec![InputEvent::PointerMove(PointerMove { x: 40_000, y: 0 })],
    });
    let r = h.req(vm, Some(run), bad);
    denied(ErrorCode::InvalidArgument, h.core.submit(&Caller::User, r));
}

#[test]
fn unsupported_host_capabilities_are_explicit() {
    let mut cfg = config();
    cfg.host.accelerator_available = false;
    cfg.host.screenshot_available = false;
    let mut h = Harness::with_config(cfg);
    let vm = h.create_vm("a");
    let r = h.req(vm, None, VmAction::Start);
    let e = h.core.submit(&Caller::User, r).unwrap_err();
    assert_eq!(e.code, ErrorCode::Unsupported);
    assert_eq!(
        e.details.unwrap().get("capability").map(String::as_str),
        Some("vm.start")
    );
    assert!(h.backend.calls.is_empty());
}

#[test]
fn running_vm_limit_is_enforced() {
    let mut h = Harness::new();
    let a = h.create_vm("a");
    let b = h.create_vm("b");
    h.start(a);
    let r = h.req(b, None, VmAction::Start);
    denied(ErrorCode::ResourceLimit, h.core.submit(&Caller::User, r));
}

#[test]
fn restart_reconciles_persisted_running_state() {
    let mut h = Harness::new();
    let vm = h.create_vm("a");
    h.start(vm);
    assert_eq!(
        h.store.0.borrow().vms[&vm].persisted_state,
        VmState::Running
    );
    // Nuovo supervisor sullo stesso catalogo: lo stato persistito non basta.
    let mut h2 = Harness::with_store(config(), h.store.clone());
    h2.core.load_catalog().unwrap();
    assert_eq!(
        h2.core.vm_status(&Caller::User, vm).unwrap().state,
        VmState::Recovering
    );
    let r = h2.req(vm, None, VmAction::Start);
    denied(ErrorCode::StateConflict, h2.core.submit(&Caller::User, r));
    h2.core.finish_recovery(vm).unwrap();
    assert_eq!(
        h2.core.vm_status(&Caller::User, vm).unwrap().state,
        VmState::Stopped
    );
}

#[test]
fn guard_table_matches_architecture() {
    use OperationKind::*;
    for s in VmState::ALL {
        assert_eq!(action_allowed_in(Start, s), s == VmState::Stopped);
        assert_eq!(action_allowed_in(Input, s), s == VmState::Running);
        assert_eq!(action_allowed_in(ForceStop, s), s.has_process());
        assert!(action_allowed_in(EmergencyStop, s));
        if s == VmState::Recovering {
            assert!(
                [Start, Stop, Reset, Screenshot, Input, Snapshot, Restore]
                    .iter()
                    .all(|k| !action_allowed_in(*k, s)),
                "nessuna mutazione in recovering"
            );
        }
    }
}

// ----- sessioni (ROADMAP 7.4 "Sessioni", "VM") -----------------------------

#[test]
fn missing_wrong_expired_revoked_or_restarted_token_is_denied_without_effect() {
    let mut h = Harness::new();
    let vm = h.create_vm("a");
    let run = h.start(vm);
    let calls_before = h.backend.calls.len();
    let (sid, agent) = h.session(&[vm], &ALL_CAPS, 1);

    // Token assente.
    let none = Caller::Agent {
        session_id: sid,
        token: None,
    };
    let r = h.req(vm, Some(run), VmAction::Screenshot);
    denied(ErrorCode::Unauthenticated, h.core.submit(&none, r));
    // Token errato.
    let wrong = Caller::Agent {
        session_id: sid,
        token: Some(SessionToken::from_bytes([2; 32])),
    };
    let r = h.req(vm, Some(run), VmAction::Screenshot);
    denied(ErrorCode::Unauthenticated, h.core.submit(&wrong, r));
    // Sessione inesistente.
    let ghost = Caller::Agent {
        session_id: SessionId::from_unix_millis(9, [9; 10]),
        token: Some(SessionToken::from_bytes([1; 32])),
    };
    let r = h.req(vm, Some(run), VmAction::Screenshot);
    denied(ErrorCode::Unauthenticated, h.core.submit(&ghost, r));
    // Valido.
    let r = h.req(vm, Some(run), VmAction::Screenshot);
    h.core.submit(&agent, r).unwrap();
    h.drain(vm);
    // Revocato.
    h.core
        .revoke_session(sid, TerminationCause::Revoked)
        .unwrap();
    let r = h.req(vm, Some(run), VmAction::Screenshot);
    denied(ErrorCode::Unauthenticated, h.core.submit(&agent, r));
    denied(
        ErrorCode::Unauthenticated,
        h.core
            .renew_lease(sid, &SessionToken::from_bytes([1; 32]), 10_000),
    );

    // Scaduto (grant).
    let (sid2, agent2) = h.session(&[vm], &ALL_CAPS, 3);
    h.clock.advance(3_600_001);
    let r = h.req(vm, None, VmAction::Stop);
    denied(ErrorCode::SessionExpired, h.core.submit(&agent2, r));
    let _ = sid2;

    // Token riusato dopo riavvio del supervisor: nessuna sessione sopravvive.
    let mut h2 = Harness::with_store(config(), h.store.clone());
    h2.core.load_catalog().unwrap();
    let r = h2.req(vm, None, VmAction::Stop);
    denied(ErrorCode::Unauthenticated, h2.core.submit(&agent, r));

    // Nessun effetto: solo lo screenshot autorizzato ha raggiunto il backend.
    let new_calls = &h.backend.calls[calls_before..];
    assert_eq!(new_calls.len(), 1);
    assert!(matches!(new_calls[0], Call::Screenshot(..)));
    assert!(h2.backend.calls.is_empty());
}

#[test]
fn session_on_a_cannot_touch_b() {
    let mut h = Harness::new();
    let a = h.create_vm("a");
    let b = h.create_vm("b");
    let mut cfg = config();
    cfg.global_limits.max_running_vms = 2;
    let _ = cfg;
    let run_a = h.start(a);
    let (_sid, agent) = h.session(&[a], &ALL_CAPS, 1);
    // Stato, screenshot e input su B negati; nessun dato di B.
    denied(ErrorCode::PermissionDenied, h.core.vm_status(&agent, b));
    let r = h.req(b, Some(run_a), VmAction::Screenshot);
    denied(ErrorCode::PermissionDenied, h.core.submit(&agent, r));
    let act = h.input(b, run_a, 1);
    let r = h.req(b, Some(run_a), act);
    denied(ErrorCode::PermissionDenied, h.core.submit(&agent, r));
    // VM inesistente: stesso errore (non rivela l'esistenza).
    let ghost = VmId::from_unix_millis(1, [0x77; 10]);
    denied(ErrorCode::PermissionDenied, h.core.vm_status(&agent, ghost));
    // L'elenco contiene solo A.
    let list = h.core.list_vms(&agent).unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].vm_id, a);
    assert!(h.core.vm_status(&agent, a).is_ok());
    assert_eq!(h.backend.count(|c| matches!(c, Call::Screenshot(..))), 0);
}

#[test]
fn grant_capabilities_are_enforced() {
    let mut h = Harness::new();
    let vm = h.create_vm("a");
    let run = h.start(vm);
    let (_sid, agent) = h.session(&[vm], &[Capability::VmScreenshot, Capability::VmStatus], 1);
    let r = h.req(vm, Some(run), VmAction::Reset);
    denied(ErrorCode::PermissionDenied, h.core.submit(&agent, r));
    let r = h.req(vm, None, VmAction::Stop);
    denied(ErrorCode::PermissionDenied, h.core.submit(&agent, r));
    let r = h.req(vm, Some(run), VmAction::Screenshot);
    h.core.submit(&agent, r).unwrap();
    // La policy VM restringe anche ciò che il grant concede.
    let mut layer = PolicyLayer::permissive();
    layer.capabilities.remove(&Capability::VmScreenshot);
    h.core.set_vm_policy(vm, layer);
    let r = h.req(vm, Some(run), VmAction::Screenshot);
    denied(ErrorCode::PermissionDenied, h.core.submit(&agent, r));
    // Anche il già accodato viene scartato alla seconda verifica.
    let rec = h.run_next(vm);
    assert!(rec.is_none());
    assert_eq!(h.backend.count(|c| matches!(c, Call::Screenshot(..))), 0);
}

#[test]
fn quotas_are_enforced() {
    let mut h = Harness::new();
    let vm = h.create_vm("a");
    let run = h.start(vm);
    h.screenshot_geometry(vm, run);
    let spec = vm_core::session::SessionSpec {
        vm_allowlist: [vm].into_iter().collect(),
        capabilities: ALL_CAPS.into_iter().collect(),
        quotas: Quotas {
            max_operations: 3,
            max_input_events: 5,
            max_screenshots: 1,
            max_screenshot_bytes: 10_000,
            max_queued_operations: 2,
        },
        expires_at: UtcTimestamp::from_unix_millis(T0 + 60_000).unwrap(),
        lease_millis: 10_000,
    };
    let (grant, _) = h
        .core
        .create_session(spec, SessionToken::from_bytes([5; 32]))
        .unwrap();
    let agent = Caller::Agent {
        session_id: grant.session_id,
        token: Some(SessionToken::from_bytes([5; 32])),
    };
    let r = h.req(vm, Some(run), VmAction::Screenshot);
    h.core.submit(&agent, r).unwrap();
    let r = h.req(vm, Some(run), VmAction::Screenshot);
    denied(ErrorCode::ResourceLimit, h.core.submit(&agent, r));
    let a = h.input(vm, run, 6);
    let r = h.req(vm, Some(run), a);
    denied(ErrorCode::ResourceLimit, h.core.submit(&agent, r));
    let a = h.input(vm, run, 2);
    let r = h.req(vm, Some(run), a);
    h.core.submit(&agent, r).unwrap();
    // Coda piena (2).
    let a = h.input(vm, run, 1);
    let r = h.req(vm, Some(run), a);
    denied(ErrorCode::ResourceLimit, h.core.submit(&agent, r));
    h.drain(vm);
    let a = h.input(vm, run, 1);
    let r = h.req(vm, Some(run), a);
    h.core.submit(&agent, r).unwrap();
    // Quota di operazioni (3) esaurita.
    let a = h.input(vm, run, 1);
    let r = h.req(vm, Some(run), a);
    denied(ErrorCode::ResourceLimit, h.core.submit(&agent, r));
}

#[test]
fn pause_blocks_mutations_but_not_observation() {
    let mut h = Harness::new();
    let vm = h.create_vm("a");
    let run = h.start(vm);
    h.screenshot_geometry(vm, run);
    let (sid, agent) = h.session(&[vm], &ALL_CAPS, 1);
    let a = h.input(vm, run, 1);
    let r = h.req(vm, Some(run), a);
    let queued = r.operation_id;
    h.core.submit(&agent, r).unwrap();
    h.core.pause_session(sid).unwrap();
    // L'input accodato prima della pausa non viene inoltrato.
    assert!(h.run_next(vm).is_none());
    let op = h.core.operation(&Caller::User, queued).unwrap();
    assert_eq!(op.status, OperationStatus::Discarded);
    let a = h.input(vm, run, 1);
    let r = h.req(vm, Some(run), a);
    denied(ErrorCode::PermissionDenied, h.core.submit(&agent, r));
    let r = h.req(vm, Some(run), VmAction::Screenshot);
    h.core.submit(&agent, r).unwrap();
    h.core.resume_session(sid).unwrap();
    let a = h.input(vm, run, 1);
    let r = h.req(vm, Some(run), a);
    h.core.submit(&agent, r).unwrap();
    h.drain(vm);
    assert_eq!(h.backend.count(|c| matches!(c, Call::Input(..))), 1);
}

#[test]
fn revocation_between_receipt_and_forwarding_discards_without_effect() {
    let mut h = Harness::new();
    let vm = h.create_vm("a");
    let run = h.start(vm);
    h.screenshot_geometry(vm, run);
    let (sid, agent) = h.session(&[vm], &ALL_CAPS, 1);
    let mut ids = Vec::new();
    for _ in 0..5 {
        let a = h.input(vm, run, 1);
        let r = h.req(vm, Some(run), a);
        ids.push(r.operation_id);
        h.core.submit(&agent, r).unwrap();
    }
    let forwarded = h.core.begin_dispatch(vm).unwrap().unwrap();
    let t = h
        .core
        .revoke_session(sid, TerminationCause::Revoked)
        .unwrap();
    assert_eq!(t.report.already_forwarded, vec![forwarded.operation_id]);
    assert_eq!(t.report.discarded, ids[1..].to_vec());
    assert!(t.emergency_stops.is_empty(), "la revoca non arresta la VM");
    assert!(h.core.begin_dispatch(vm).unwrap().is_none());
    let ev = h.audit_events();
    let term = ev
        .iter()
        .find(|e| e.action == AuditAction::SessionTerminate)
        .unwrap();
    assert_eq!(term.termination, Some(TerminationCause::Revoked));
    assert_eq!(
        ev.iter()
            .filter(|e| e.outcome == AuditOutcome::Discarded)
            .count(),
        4
    );
}

// ----- Emergency Stop e lifecycle (ROADMAP 7.3, 7.4 "Revoca", "Lifecycle") --

#[test]
fn flood_during_emergency_stop_forwards_nothing_after_revocation() {
    let mut h = Harness::new();
    let vm = h.create_vm("a");
    let run = h.start(vm);
    h.screenshot_geometry(vm, run);
    let (sid, agent) = h.session(&[vm], &ALL_CAPS, 1);
    for _ in 0..100 {
        let a = h.input(vm, run, 1);
        let r = h.req(vm, Some(run), a);
        h.core.submit(&agent, r).unwrap();
    }
    // Una richiesta inoltrata e bloccata (QMP non risponde).
    let in_flight = h.core.begin_dispatch(vm).unwrap().unwrap();
    let inputs_before = h.backend.count(|c| matches!(c, Call::Input(..)));

    let (plan, killed) = h
        .core
        .emergency_stop_with(Actor::User, vm, &mut h.backend)
        .unwrap();
    killed.unwrap();
    assert_eq!(plan.run_id, Some(run));
    assert_eq!(plan.terminated_sessions, vec![sid]);
    assert_eq!(plan.report.already_forwarded, vec![in_flight.operation_id]);
    assert_eq!(plan.report.discarded.len(), 99);

    // Il flood continua dopo la revoca: tutto negato, niente inoltrato.
    for _ in 0..50 {
        let a = h.input(vm, run, 1);
        let r = h.req(vm, Some(run), a);
        assert!(h.core.submit(&agent, r).is_err());
    }
    assert!(h.core.begin_dispatch(vm).unwrap().is_none());
    assert_eq!(
        h.backend.count(|c| matches!(c, Call::Input(..))),
        inputs_before,
        "nessun nuovo inoltro dopo la revoca"
    );
    assert_eq!(h.backend.count(|c| matches!(c, Call::ForceStop(..))), 1);
    let st = h.core.vm_status(&Caller::User, vm).unwrap();
    assert_eq!(st.state, VmState::Stopped, "fine processo osservata");
    // La richiesta già inoltrata si chiude senza riattivare nulla.
    h.core
        .finish_dispatch(vm, in_flight.operation_id, Ok(DispatchOutcome::Done))
        .unwrap();
    assert_eq!(
        h.core.vm_status(&Caller::User, vm).unwrap().state,
        VmState::Stopped
    );
    let run_rec = h.store.0.borrow().runs[&run].clone();
    assert_eq!(run_rec.end_reason, Some(RunEndReason::EmergencyStop));
}

#[test]
fn emergency_stop_does_not_declare_completion_without_process_exit() {
    let mut h = Harness::new();
    let vm = h.create_vm("a");
    let run = h.start(vm);
    let plan = h.core.emergency_stop(Actor::User, vm).unwrap();
    assert_eq!(plan.run_id, Some(run));
    assert_eq!(
        h.core.vm_status(&Caller::User, vm).unwrap().state,
        VmState::Stopping
    );
    // Nessuna nuova operazione ammessa mentre è in stopping, salvo force-stop.
    let r = h.req(vm, Some(run), VmAction::Screenshot);
    denied(ErrorCode::StateConflict, h.core.submit(&Caller::User, r));
    h.core
        .observe(
            vm,
            BackendObservation::ProcessExited {
                run_id: run,
                expected: true,
            },
        )
        .unwrap();
    assert_eq!(
        h.core.vm_status(&Caller::User, vm).unwrap().state,
        VmState::Stopped
    );
    // Kill fallito: stato failed, errore restituito.
    let run2 = {
        h.core.begin_recovery(vm).unwrap_err();
        h.start(vm)
    };
    h.backend.fail_next = Some(PublicError::infrastructure("kill fallito"));
    let (_plan, killed) = h
        .core
        .emergency_stop_with(Actor::User, vm, &mut h.backend)
        .unwrap();
    assert!(killed.is_err());
    let st = h.core.vm_status(&Caller::User, vm).unwrap();
    assert_eq!(st.state, VmState::Failed);
    assert_eq!(st.run_id, Some(run2));
}

#[test]
fn lease_expiry_revokes_and_requests_emergency_stop() {
    let mut h = Harness::new();
    let vm = h.create_vm("a");
    let run = h.start(vm);
    let (sid, agent) = h.session(&[vm], &ALL_CAPS, 1);
    let token = SessionToken::from_bytes([1; 32]);
    // Rinnovi regolari mantengono viva la sessione.
    for _ in 0..5 {
        h.clock.advance(9_000);
        assert!(h.core.tick().unwrap().is_empty());
        h.core.renew_lease(sid, &token, 10_000).unwrap();
    }
    // Il client smette di rinnovare.
    h.clock.advance(10_000);
    let r = h.req(vm, Some(run), VmAction::Screenshot);
    denied(ErrorCode::SessionExpired, h.core.submit(&agent, r));
    let terms = h.core.tick().unwrap();
    assert_eq!(terms.len(), 1);
    assert_eq!(terms[0].cause, TerminationCause::LeaseExpired);
    assert_eq!(terms[0].emergency_stops.len(), 1);
    assert_eq!(terms[0].emergency_stops[0].run_id, Some(run));
    // Rinnovo tardivo rifiutato: serve una nuova autorizzazione.
    denied(
        ErrorCode::SessionExpired,
        h.core.renew_lease(sid, &token, 10_000),
    );
    denied(ErrorCode::StateConflict, h.core.resume_session(sid));
    assert!(
        h.core.tick().unwrap().is_empty(),
        "nessuna doppia terminazione"
    );
}

#[test]
fn client_loss_and_ui_close_terminate_with_stop() {
    for cause in [
        TerminationCause::ClientLost,
        TerminationCause::UiClosed,
        TerminationCause::HostResumed,
    ] {
        let mut h = Harness::new();
        let vm = h.create_vm("a");
        h.start(vm);
        let (sid, agent) = h.session(&[vm], &ALL_CAPS, 1);
        let t = h.core.terminate_session_with_stop(sid, cause).unwrap();
        assert_eq!(t.emergency_stops.len(), 1);
        let r = h.req(vm, None, VmAction::Stop);
        denied(ErrorCode::Unauthenticated, h.core.submit(&agent, r));
    }
}

#[test]
fn session_creation_limits() {
    let mut h = Harness::new();
    let vm = h.create_vm("a");
    h.session(&[vm], &ALL_CAPS, 1);
    let spec = vm_core::session::SessionSpec {
        vm_allowlist: [vm].into_iter().collect(),
        capabilities: ALL_CAPS.into_iter().collect(),
        quotas: Quotas::UNBOUNDED,
        expires_at: UtcTimestamp::from_unix_millis(T0 + 60_000).unwrap(),
        lease_millis: 10_000,
    };
    denied(
        ErrorCode::ResourceLimit,
        h.core
            .create_session(spec.clone(), SessionToken::from_bytes([2; 32])),
    );
    let mut h = Harness::new();
    let vm2 = h.create_vm("b");
    let mut s = spec.clone();
    s.vm_allowlist = [vm2].into_iter().collect();
    s.lease_millis = 10_001;
    denied(
        ErrorCode::InvalidArgument,
        h.core
            .create_session(s.clone(), SessionToken::from_bytes([2; 32])),
    );
    s.lease_millis = 10_000;
    s.vm_allowlist = Default::default();
    denied(
        ErrorCode::InvalidArgument,
        h.core
            .create_session(s.clone(), SessionToken::from_bytes([2; 32])),
    );
    // VmId inesistente: con fake clock e contatore gli id sono deterministici,
    // quindi `vm` (del primo harness) esisterebbe anche in questo.
    let ghost = VmId::from_unix_millis(1, [0x42; 10]);
    s.vm_allowlist = [ghost].into_iter().collect();
    denied(
        ErrorCode::NotFound,
        h.core
            .create_session(s.clone(), SessionToken::from_bytes([2; 32])),
    );
    s.vm_allowlist = [vm2].into_iter().collect();
    s.expires_at = UtcTimestamp::from_unix_millis(T0).unwrap();
    denied(
        ErrorCode::InvalidArgument,
        h.core.create_session(s, SessionToken::from_bytes([2; 32])),
    );
}

#[test]
fn agent_sees_only_its_own_operations() {
    let mut h = Harness::new();
    let vm = h.create_vm("a");
    let run = h.start(vm);
    let (_sid, agent) = h.session(&[vm], &ALL_CAPS, 1);
    let r = h.req(vm, Some(run), VmAction::Screenshot);
    let mine = r.operation_id;
    h.core.submit(&agent, r).unwrap();
    let r = h.req(vm, Some(run), VmAction::Screenshot);
    let users = r.operation_id;
    h.core.submit(&Caller::User, r).unwrap();
    assert!(h.core.operation(&agent, mine).is_ok());
    denied(ErrorCode::NotFound, h.core.operation(&agent, users));
}

// ----- audit ----------------------------------------------------------------

#[test]
fn audit_is_ordered_and_contains_no_secrets() {
    let mut h = Harness::new();
    let vm = h.create_vm("segreti");
    let run = h.start(vm);
    h.screenshot_geometry(vm, run);
    let (sid, agent) = h.session(&[vm], &ALL_CAPS, 0x5a);
    let a = VmAction::Input(InputBatch {
        vm_id: vm,
        run_id: run,
        geometry: DisplayGeometry {
            width: 1280,
            height: 800,
        },
        events: vec![InputEvent::Key(KeyAction {
            qcode: QCode::parse("p").unwrap(),
            down: true,
        })],
    });
    let r = h.req(vm, Some(run), a);
    h.core.submit(&agent, r).unwrap();
    h.drain(vm);
    let bad = Caller::Agent {
        session_id: sid,
        token: Some(SessionToken::from_bytes([0x6b; 32])),
    };
    let r = h.req(vm, Some(run), VmAction::Screenshot);
    let _ = h.core.submit(&bad, r);
    h.stop(vm);

    let ev = h.audit_events();
    assert!(ev.windows(2).all(|w| w[0].seq < w[1].seq));
    assert!(ev.iter().any(|e| e.outcome == AuditOutcome::Denied));
    let json = serde_json::to_string(&ev).unwrap();
    for secret in ["5a5a5a5a", "6b6b6b6b", "media/", "qcode", "\"p\""] {
        assert!(!json.contains(secret), "audit contiene {secret}");
    }
    let actions: Vec<_> = ev.iter().map(|e| e.action).collect();
    for needed in [
        AuditAction::VmCreate,
        AuditAction::VmStart,
        AuditAction::VmStop,
        AuditAction::SessionCreate,
        AuditAction::VmInput,
    ] {
        assert!(actions.contains(&needed), "{needed:?} mancante");
    }
}

#[test]
fn reported_screenshot_must_match_request() {
    let mut h = Harness::new();
    let vm = h.create_vm("a");
    let run = h.start(vm);
    let r = h.req(vm, Some(run), VmAction::Screenshot);
    h.core.submit(&Caller::User, r).unwrap();
    let d = h.core.begin_dispatch(vm).unwrap().unwrap();
    let other_vm = VmId::from_unix_millis(1, [3; 10]);
    let shot = Screenshot {
        vm_id: other_vm,
        run_id: run,
        geometry: DisplayGeometry {
            width: 800,
            height: 600,
        },
        format: ImageFormat::Png,
        sha256: Sha256Digest::from_bytes([0; 32]),
        byte_len: 10,
        captured_at: UtcTimestamp::from_unix_millis(T0).unwrap(),
    };
    let rec = h
        .core
        .finish_dispatch(vm, d.operation_id, Ok(DispatchOutcome::Screenshot(shot)))
        .unwrap();
    assert_eq!(rec.status, OperationStatus::Failed);
    assert_eq!(h.core.vm_status(&Caller::User, vm).unwrap().geometry, None);
}

// ----- impostazioni della VM: risorse e limiti (ADR-0017) ------------------

#[test]
fn update_vm_config_changes_resources_and_limits_of_a_stopped_vm() {
    let mut h = Harness::new();
    let vm = h.create_vm("modificabile");
    let st = h.core.vm_status(&Caller::User, vm).unwrap();
    assert_eq!(st.config_revision, 1);
    assert_eq!(st.config.limits, VmLimits::default());
    let mut cfg = st.config.clone();
    cfg.vcpus = 4;
    cfg.memory_mib = 6144;
    cfg.disk_gib += 8;
    cfg.limits = VmLimits {
        cpu_max_percent: 50,
        memory_margin_mib: 2048,
        storage_budget_gib: Some(20),
    };
    let rec = h
        .core
        .update_vm_config(&Caller::User, vm, 1, cfg.clone())
        .unwrap();
    assert_eq!(rec.config_revision, 2);
    assert_eq!(rec.config, cfg);
    let st = h.core.vm_status(&Caller::User, vm).unwrap();
    assert_eq!((st.config_revision, st.config), (2, cfg));
    assert!(
        h.audit_events()
            .iter()
            .any(|e| e.action == AuditAction::VmConfigure
                && e.outcome == AuditOutcome::Succeeded
                && e.vm_id == Some(vm))
    );
}

#[test]
fn update_vm_config_rejections_have_no_effect() {
    let mut h = Harness::new();
    let vm = h.create_vm("rifiuti");
    let base = h.core.vm_status(&Caller::User, vm).unwrap().config;
    // Revisione superata.
    denied(
        ErrorCode::StateConflict,
        h.core.update_vm_config(&Caller::User, vm, 7, base.clone()),
    );
    // Disco ridotto.
    let mut c = base.clone();
    c.disk_gib -= 1;
    denied(
        ErrorCode::InvalidArgument,
        h.core.update_vm_config(&Caller::User, vm, 1, c),
    );
    // Limiti fuori intervallo.
    for limits in [
        VmLimits {
            cpu_max_percent: 0,
            ..VmLimits::default()
        },
        VmLimits {
            cpu_max_percent: 101,
            ..VmLimits::default()
        },
        VmLimits {
            memory_margin_mib: 10,
            ..VmLimits::default()
        },
        VmLimits {
            storage_budget_gib: Some(0),
            ..VmLimits::default()
        },
    ] {
        let mut c = base.clone();
        c.limits = limits;
        denied(
            ErrorCode::InvalidArgument,
            h.core.update_vm_config(&Caller::User, vm, 1, c),
        );
    }
    // Risorse oltre i limiti effettivi.
    let mut c = base.clone();
    c.vcpus = 64;
    denied(
        ErrorCode::ResourceLimit,
        h.core.update_vm_config(&Caller::User, vm, 1, c),
    );
    // Sessione agente: nessuna modifica.
    let (_, agent) = h.session(&[vm], &[Capability::VmStart], 9);
    denied(
        ErrorCode::PermissionDenied,
        h.core.update_vm_config(&agent, vm, 1, base.clone()),
    );
    // VM accesa.
    h.start(vm);
    denied(
        ErrorCode::StateConflict,
        h.core.update_vm_config(&Caller::User, vm, 1, base.clone()),
    );
    let st = h.core.vm_status(&Caller::User, vm).unwrap();
    assert_eq!((st.config_revision, st.config), (1, base));
    assert!(
        h.audit_events()
            .iter()
            .filter(|e| e.action == AuditAction::VmConfigure)
            .all(|e| e.outcome == AuditOutcome::Denied)
    );
}

//! Riconciliazione dello stato dalle osservazioni del backend
//! (`BackendObservation`): processo e QMP prevalgono sullo stato persistito e
//! sugli esiti tardivi del backend; le osservazioni di esecuzioni non correnti
//! sono ignorate. Backend e osservazioni sono simulati (nessun QEMU reale).

mod common;

use common::*;
use proptest::prelude::*;
use vm_core::DispatchOutcome;
use vm_core::proto::*;

fn state(h: &mut Harness, vm: VmId) -> VmState {
    h.core.vm_status(&Caller::User, vm).unwrap().state
}

fn run_of(h: &Harness, run: RunId) -> RunRecord {
    h.store.0.borrow().runs[&run].clone()
}

/// Avvio inoltrato ma senza esito del backend: VM in `starting`.
fn begin_start(h: &mut Harness, vm: VmId) -> (OperationId, RunId) {
    let r = h.req(vm, None, VmAction::Start);
    h.core.submit(&Caller::User, r).unwrap();
    let d = h.core.begin_dispatch(vm).unwrap().unwrap();
    assert_eq!(d.action, VmAction::Start);
    (d.operation_id, d.run_id)
}

#[test]
fn qmp_ready_promotes_starting_before_the_backend_result() {
    let mut h = Harness::new();
    let vm = h.create_vm("a");
    let (op, run) = begin_start(&mut h, vm);
    assert_eq!(state(&mut h, vm), VmState::Starting);
    assert!(
        h.core
            .observe(vm, BackendObservation::ProcessSpawned { run_id: run })
            .unwrap()
    );
    assert_eq!(state(&mut h, vm), VmState::Starting, "processo senza QMP");
    assert!(
        h.core
            .observe(vm, BackendObservation::QmpReady { run_id: run })
            .unwrap()
    );
    assert_eq!(state(&mut h, vm), VmState::Running);
    // Esito tardivo del backend: nessun cambiamento.
    let rec = h
        .core
        .finish_dispatch(vm, op, Ok(DispatchOutcome::Done))
        .unwrap();
    assert_eq!(rec.status, OperationStatus::Succeeded);
    assert_eq!(state(&mut h, vm), VmState::Running);
    // QMP pronto ripetuto: nessun effetto.
    assert!(
        h.core
            .observe(vm, BackendObservation::QmpReady { run_id: run })
            .unwrap()
    );
    assert_eq!(state(&mut h, vm), VmState::Running);
}

#[test]
fn start_failed_observation_wins_over_a_late_success() {
    let mut h = Harness::new();
    let vm = h.create_vm("a");
    let (op, run) = begin_start(&mut h, vm);
    h.core
        .observe(vm, BackendObservation::StartFailed { run_id: run })
        .unwrap();
    let st = h.core.vm_status(&Caller::User, vm).unwrap();
    assert_eq!(st.state, VmState::Failed);
    assert_eq!(st.run_id, None);
    assert_eq!(run_of(&h, run).end_reason, Some(RunEndReason::StartFailed));
    // Il backend riporta "avviato" dopo: l'esecuzione non è più corrente.
    h.core
        .finish_dispatch(vm, op, Ok(DispatchOutcome::Done))
        .unwrap();
    assert_eq!(state(&mut h, vm), VmState::Failed);
    // Ulteriori osservazioni della stessa esecuzione sono ignorate.
    assert!(
        !h.core
            .observe(vm, BackendObservation::QmpReady { run_id: run })
            .unwrap()
    );
    assert_eq!(state(&mut h, vm), VmState::Failed);
}

#[test]
fn unexpected_exit_while_running_fails_and_discards_the_queue() {
    let mut h = Harness::new();
    let vm = h.create_vm("a");
    let run = h.start(vm);
    let (_sid, agent) = h.session(&[vm], &ALL_CAPS, 1);
    let r = h.req(vm, Some(run), VmAction::Screenshot);
    let queued = r.operation_id;
    h.core.submit(&agent, r).unwrap();
    h.core
        .observe(
            vm,
            BackendObservation::ProcessExited {
                run_id: run,
                expected: false,
            },
        )
        .unwrap();
    let st = h.core.vm_status(&Caller::User, vm).unwrap();
    assert_eq!((st.state, st.run_id, st.queued), (VmState::Failed, None, 0));
    assert_eq!(st.geometry, None);
    assert_eq!(
        run_of(&h, run).end_reason,
        Some(RunEndReason::UnexpectedExit)
    );
    let op = h.core.operation(&Caller::User, queued).unwrap();
    assert_eq!(
        (op.status, op.error),
        (OperationStatus::Discarded, Some(ErrorCode::StateConflict))
    );
    assert_eq!(h.backend.count(|c| matches!(c, Call::Screenshot(..))), 0);
    // La sessione resta valida: l'agente vede lo stato `failed` ma non può riavviare.
    assert_eq!(h.core.vm_status(&agent, vm).unwrap().state, VmState::Failed);
    let r = h.req(vm, None, VmAction::Start);
    let e = h.core.submit(&agent, r).unwrap_err();
    assert_eq!(e.code, ErrorCode::StateConflict);
}

#[test]
fn exit_during_stop_is_a_clean_stop_even_if_backend_reports_late_or_fails() {
    for late in [
        Ok(DispatchOutcome::Done),
        Err(PublicError::infrastructure("timeout QMP")),
    ] {
        let mut h = Harness::new();
        let vm = h.create_vm("a");
        let run = h.start(vm);
        let r = h.req(vm, None, VmAction::Stop);
        h.core.submit(&Caller::User, r).unwrap();
        let d = h.core.begin_dispatch(vm).unwrap().unwrap();
        assert_eq!(state(&mut h, vm), VmState::Stopping);
        // Arresto non dichiarato completo senza la fine del processo.
        assert_eq!(run_of(&h, run).ended_at, None);
        // Il guest si spegne: l'uscita arriva prima dell'esito della richiesta.
        h.core
            .observe(
                vm,
                BackendObservation::ProcessExited {
                    run_id: run,
                    expected: false,
                },
            )
            .unwrap();
        assert_eq!(state(&mut h, vm), VmState::Stopped);
        assert_eq!(run_of(&h, run).end_reason, Some(RunEndReason::Stopped));
        h.core.finish_dispatch(vm, d.operation_id, late).unwrap();
        let st = h.core.vm_status(&Caller::User, vm).unwrap();
        assert_eq!((st.state, st.run_id), (VmState::Stopped, None));
        assert!(st.in_flight.is_empty());
    }
}

#[test]
fn force_stop_failure_requires_explicit_reconciliation() {
    let mut h = Harness::new();
    let vm = h.create_vm("a");
    let run = h.start(vm);
    let r = h.req(vm, None, VmAction::ForceStop);
    h.core.submit(&Caller::User, r).unwrap();
    h.backend.fail_next = Some(PublicError::infrastructure("terminazione non confermata"));
    let rec = h.run_next(vm).unwrap();
    assert_eq!(rec.status, OperationStatus::Failed);
    let st = h.core.vm_status(&Caller::User, vm).unwrap();
    assert_eq!(st.state, VmState::Failed);
    assert_eq!(
        st.run_id,
        Some(run),
        "esecuzione non chiusa senza fine osservata"
    );
    assert_eq!(run_of(&h, run).ended_at, None);
    // La fine del processo osservata chiude l'esecuzione, ma `failed` non
    // diventa `stopped` da solo: serve la riconciliazione.
    assert!(
        h.core
            .observe(
                vm,
                BackendObservation::ProcessExited {
                    run_id: run,
                    expected: true,
                },
            )
            .unwrap()
    );
    let st = h.core.vm_status(&Caller::User, vm).unwrap();
    assert_eq!((st.state, st.run_id), (VmState::Failed, None));
    assert_eq!(run_of(&h, run).end_reason, Some(RunEndReason::ForceStopped));
    let r = h.req(vm, None, VmAction::Start);
    assert_eq!(
        h.core.submit(&Caller::User, r).unwrap_err().code,
        ErrorCode::StateConflict
    );
    h.core.begin_recovery(vm).unwrap();
    h.core.finish_recovery(vm).unwrap();
    assert_eq!(state(&mut h, vm), VmState::Stopped);
    h.start(vm);
}

#[test]
fn stale_and_unknown_observations_are_ignored() {
    let mut h = Harness::new();
    let vm = h.create_vm("a");
    let stale = RunId::from_unix_millis(1, [0x33; 10]);
    // VM ferma: nessuna esecuzione corrente.
    for obs in [
        BackendObservation::ProcessSpawned { run_id: stale },
        BackendObservation::QmpReady { run_id: stale },
        BackendObservation::StartFailed { run_id: stale },
        BackendObservation::ProcessExited {
            run_id: stale,
            expected: false,
        },
    ] {
        assert!(!h.core.observe(vm, obs.clone()).unwrap(), "{obs:?}");
        assert_eq!(state(&mut h, vm), VmState::Stopped);
    }
    let run = h.start(vm);
    for obs in [
        BackendObservation::StartFailed { run_id: stale },
        BackendObservation::ProcessExited {
            run_id: stale,
            expected: false,
        },
    ] {
        assert!(!h.core.observe(vm, obs).unwrap());
        assert_eq!(state(&mut h, vm), VmState::Running);
    }
    // VM sconosciuta.
    let ghost = VmId::from_unix_millis(1, [0x44; 10]);
    let e = h
        .core
        .observe(ghost, BackendObservation::QmpReady { run_id: run })
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::NotFound);
    // Geometria: esecuzione non corrente ignorata, valori non validi rifiutati.
    let g = DisplayGeometry {
        width: 1024,
        height: 768,
    };
    assert!(!h.core.observe_geometry(vm, stale, g).unwrap());
    assert_eq!(h.core.vm_status(&Caller::User, vm).unwrap().geometry, None);
    let bad = DisplayGeometry {
        width: 0,
        height: 768,
    };
    assert_eq!(
        h.core.observe_geometry(vm, run, bad).unwrap_err().code,
        ErrorCode::InvalidArgument
    );
    assert!(h.core.observe_geometry(vm, run, g).unwrap());
    assert_eq!(
        h.core.vm_status(&Caller::User, vm).unwrap().geometry,
        Some(g)
    );
}

#[test]
fn load_catalog_never_trusts_a_persisted_process_state() {
    let store = MemStore::default();
    let mut ids = Vec::new();
    for (i, persisted) in VmState::ALL.into_iter().enumerate() {
        let id = VmId::from_unix_millis(10, [u8::try_from(i).unwrap(); 10]);
        let h = Harness::new();
        let cfg = h.vm_config("v", None);
        store.0.borrow_mut().vms.insert(
            id,
            VmRecord {
                id,
                config: cfg,
                config_revision: 1,
                persisted_state: persisted,
                created_at: UtcTimestamp::from_unix_millis(T0).unwrap(),
            },
        );
        ids.push((id, persisted));
    }
    let mut h = Harness::with_store(config(), store);
    h.core.load_catalog().unwrap();
    for (id, persisted) in ids {
        let expected = if persisted.has_process() {
            VmState::Recovering
        } else {
            persisted
        };
        let st = h.core.vm_status(&Caller::User, id).unwrap();
        assert_eq!(st.state, expected, "{persisted}");
        assert_eq!(st.run_id, None);
        assert_eq!(h.store.0.borrow().vms[&id].persisted_state, expected);
        // Nessuna osservazione è accettata senza un'esecuzione corrente.
        let any_run = RunId::from_unix_millis(10, [0x55; 10]);
        assert!(
            !h.core
                .observe(id, BackendObservation::QmpReady { run_id: any_run })
                .unwrap()
        );
    }
    assert!(h.backend.calls.is_empty());
}

// ----- proprietà: transizioni sempre ammesse dalla tabella --------------------

#[derive(Clone, Debug)]
enum Step {
    Submit(u8),
    Execute {
        fail: bool,
    },
    BeginDispatch,
    FinishOldest {
        fail: bool,
    },
    Observe {
        kind: u8,
        current: bool,
        expected: bool,
    },
    Emergency,
    BeginRecovery,
    FinishRecovery,
}

fn step() -> impl Strategy<Value = Step> {
    prop_oneof![
        (0u8..5).prop_map(Step::Submit),
        any::<bool>().prop_map(|fail| Step::Execute { fail }),
        Just(Step::BeginDispatch),
        any::<bool>().prop_map(|fail| Step::FinishOldest { fail }),
        (0u8..4, any::<bool>(), any::<bool>()).prop_map(|(kind, current, expected)| {
            Step::Observe {
                kind,
                current,
                expected,
            }
        }),
        Just(Step::Emergency),
        Just(Step::BeginRecovery),
        Just(Step::FinishRecovery),
    ]
}

fn apply(h: &mut Harness, vm: VmId, pending: &mut Vec<OperationId>, s: &Step) {
    let st = h.core.vm_status(&Caller::User, vm).unwrap();
    let stale = RunId::from_unix_millis(2, [0x66; 10]);
    match s {
        Step::Submit(k) => {
            let action = match k {
                0 => VmAction::Start,
                1 => VmAction::Stop,
                2 => VmAction::ForceStop,
                3 => VmAction::Screenshot,
                _ => VmAction::Reset,
            };
            let needs_run = matches!(action, VmAction::Screenshot | VmAction::Reset);
            let r = h.req(vm, if needs_run { st.run_id } else { None }, action);
            let _ = h.core.submit(&Caller::User, r);
        }
        Step::Execute { fail } => {
            if *fail {
                h.backend.fail_next = Some(PublicError::infrastructure("guasto simulato"));
            }
            let _ = h
                .core
                .execute_next(vm, &mut h.backend, &mut h.inspector)
                .unwrap();
            h.backend.fail_next = None;
        }
        Step::BeginDispatch => {
            if let Some(d) = h.core.begin_dispatch(vm).unwrap() {
                pending.push(d.operation_id);
            }
        }
        Step::FinishOldest { fail } => {
            if !pending.is_empty() {
                let op = pending.remove(0);
                let result = if *fail {
                    Err(PublicError::infrastructure("guasto simulato"))
                } else if h
                    .core
                    .operation(&Caller::User, op)
                    .is_ok_and(|r| r.kind == OperationKind::Screenshot)
                {
                    let run = h.core.operation(&Caller::User, op).unwrap().run_id.unwrap();
                    Ok(DispatchOutcome::Screenshot(Screenshot {
                        vm_id: vm,
                        run_id: run,
                        geometry: DisplayGeometry {
                            width: 640,
                            height: 480,
                        },
                        format: ImageFormat::Png,
                        sha256: Sha256Digest::from_bytes([1; 32]),
                        byte_len: 10,
                        captured_at: UtcTimestamp::from_unix_millis(T0).unwrap(),
                    }))
                } else {
                    Ok(DispatchOutcome::Done)
                };
                h.core.finish_dispatch(vm, op, result).unwrap();
            }
        }
        Step::Observe {
            kind,
            current,
            expected,
        } => {
            let run_id = if *current {
                st.run_id.unwrap_or(stale)
            } else {
                stale
            };
            let obs = match kind {
                0 => BackendObservation::ProcessSpawned { run_id },
                1 => BackendObservation::QmpReady { run_id },
                2 => BackendObservation::StartFailed { run_id },
                _ => BackendObservation::ProcessExited {
                    run_id,
                    expected: *expected,
                },
            };
            let applied = h.core.observe(vm, obs).unwrap();
            assert!(
                !applied || st.run_id == Some(run_id),
                "osservazione non corrente applicata"
            );
        }
        Step::Emergency => {
            let _ = h.core.emergency_stop(Actor::User, vm).unwrap();
        }
        Step::BeginRecovery => {
            let _ = h.core.begin_recovery(vm);
        }
        Step::FinishRecovery => {
            let _ = h.core.finish_recovery(vm);
        }
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    /// Qualunque sequenza di richieste, esiti (anche tardivi o falliti) e
    /// osservazioni (anche di esecuzioni non correnti) produce solo transizioni
    /// ammesse dalla tabella di `VmState`; un processo implica un'esecuzione
    /// corrente; `stopped` non ha esecuzione; al più un'esecuzione aperta.
    #[test]
    fn prop_any_sequence_respects_the_transition_table(steps in prop::collection::vec(step(), 1..60)) {
        let mut h = Harness::new();
        let vm = h.create_vm("p");
        let mut pending = Vec::new();
        for s in &steps {
            apply(&mut h, vm, &mut pending, s);
            let st = h.core.vm_status(&Caller::User, vm).unwrap();
            if st.state.has_process() {
                prop_assert!(st.run_id.is_some(), "{:?} senza run_id dopo {:?}", st.state, s);
            }
            if st.state == VmState::Stopped {
                prop_assert_eq!(st.run_id, None);
            }
            let store = h.store.0.borrow();
            let open = store.runs.values().filter(|r| r.ended_at.is_none()).count();
            prop_assert!(open <= 1, "{} esecuzioni aperte", open);
            if let Some(run) = st.run_id {
                prop_assert!(store.runs.contains_key(&run));
            }
        }
        let store = h.store.0.borrow();
        let mut prev = VmState::Stopped;
        for (id, s) in &store.state_log {
            prop_assert_eq!(*id, vm);
            prop_assert!(prev.can_transition_to(*s), "transizione {} → {} non ammessa", prev, s);
            prev = *s;
        }
        // Ogni avvio del backend corrisponde a un'esecuzione registrata.
        for c in &h.backend.calls {
            if let Call::Start(_, run, _) = c {
                prop_assert!(store.runs.contains_key(run));
            }
        }
        let ev = h.audit_events();
        prop_assert!(ev.windows(2).all(|w| w[1].seq == w[0].seq + 1));
    }
}

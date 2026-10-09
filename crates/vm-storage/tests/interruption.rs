//! Interruzione delle operazioni composte attraverso le API di alto livello:
//! il produttore dei file (in prodotto `qemu-img`, qui una closure) fallisce a
//! metà di una promozione di baseline o di una preparazione di esecuzione.
//! Completa `journal.rs`, che simula i crash costruendo gli intenti a mano.
//! In coda: riconciliazione che non riesce a risolvere un intento ("da
//! verificare").

mod common;

use std::path::Path;

use vm_storage::{
    BaselineId, Catalog, IntentState, Lifecycle, PromoteBaseline, RecoveryOutcome, StorageError,
    VmId,
};

fn req(vm: &VmId) -> PromoteBaseline {
    PromoteBaseline {
        vm_id: *vm,
        label: "seconda baseline".into(),
        clean_shutdown: true,
        protected: false,
        virtual_size_bytes: 1 << 30,
    }
}

fn producer_failure() -> StorageError {
    StorageError::Unsupported("produttore interrotto (simulato)")
}

/// Nessun file sotto `vms/<vm>/base` oltre alle baseline indicate, `tmp/`
/// vuota, nessun intento aperto, VM non occupata.
fn assert_clean(dir: &Path, cat: &mut Catalog, vm: &VmId, baselines: &[BaselineId]) {
    assert!(
        cat.open_intents().unwrap().is_empty(),
        "nessun intento aperto"
    );
    assert_eq!(
        std::fs::read_dir(dir.join("tmp")).unwrap().count(),
        0,
        "tmp/ vuota"
    );
    let base_dir = dir.join("vms").join(vm.to_string()).join("base");
    let mut found: Vec<String> = match std::fs::read_dir(&base_dir) {
        Ok(it) => it
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect(),
        Err(_) => Vec::new(),
    };
    found.sort();
    let mut want: Vec<String> = baselines.iter().map(ToString::to_string).collect();
    want.sort();
    assert_eq!(found, want, "solo le baseline registrate su disco");
    let rows = cat.read(|tx| tx.baselines_of_vm(vm)).unwrap();
    assert_eq!(
        rows.len(),
        baselines.len(),
        "solo le baseline registrate nel DB"
    );
}

#[test]
fn promotion_interrupted_while_writing_the_disk_leaves_nothing() {
    let dir = common::tempdir();
    let mut cat = Catalog::open(dir.path()).unwrap();
    let vm = common::new_vm(&mut cat, "vm");
    let err = cat
        .promote_baseline(
            &req(&vm),
            |p| {
                // File parziale, poi interruzione.
                common::write_bytes(p, b"QFI\xfb parziale")?;
                Err(producer_failure())
            },
            None,
        )
        .unwrap_err();
    assert!(matches!(err, StorageError::Unsupported(_)), "{err:?}");
    assert_clean(dir.path(), &mut cat, &vm, &[]);
    // La VM non resta occupata: una promozione successiva riesce.
    let b = common::baseline(&mut cat, &vm, false);
    assert_clean(dir.path(), &mut cat, &vm, &[b]);
}

#[test]
fn promotion_interrupted_while_writing_vars_removes_the_published_disk() {
    let dir = common::tempdir();
    let mut cat = Catalog::open(dir.path()).unwrap();
    let vm = common::new_vm(&mut cat, "vm");
    let first = common::baseline(&mut cat, &vm, true);
    let first_hash = common::base_hash(&mut cat, &first);
    let err = cat
        .promote_baseline(
            &req(&vm),
            |p| common::write_bytes(p, b"QFI\xfb seconda base completa"),
            Some(Box::new(|p: &Path| {
                common::write_bytes(p, b"OVMF parziale")?;
                Err(producer_failure())
            })),
        )
        .unwrap_err();
    assert!(matches!(err, StorageError::Unsupported(_)), "{err:?}");
    // Il disco della seconda baseline era già pubblicato (intento `applied`):
    // l'annullamento lo rimuove insieme alla directory.
    assert_clean(dir.path(), &mut cat, &vm, &[first]);
    assert_eq!(common::base_hash(&mut cat, &first), first_hash);
    assert!(
        cat.verify_baseline_manifest(&first, true)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn preparation_interrupted_while_writing_the_overlay_leaves_nothing() {
    let dir = common::tempdir();
    let mut cat = Catalog::open(dir.path()).unwrap();
    let vm = common::new_vm(&mut cat, "vm");
    let b = common::baseline(&mut cat, &vm, true);
    let hash = common::base_hash(&mut cat, &b);
    let err = cat
        .prepare_run_from_baseline(&vm, &b, |tmp, base| {
            assert!(base.is_file());
            common::write_bytes(tmp, b"QFI\xfb overlay parziale")?;
            Err(producer_failure())
        })
        .unwrap_err();
    assert!(matches!(err, StorageError::Unsupported(_)), "{err:?}");
    assert_clean(dir.path(), &mut cat, &vm, &[b]);
    let overlays = dir.path().join("vms").join(vm.to_string()).join("overlays");
    let n = std::fs::read_dir(&overlays)
        .map(|it| it.count())
        .unwrap_or(0);
    assert_eq!(n, 0, "nessun overlay");
    let vm_rec = cat.read(|tx| tx.vm(&vm)).unwrap();
    assert_eq!(vm_rec.lifecycle, Lifecycle::Stopped);
    assert_eq!(common::base_hash(&mut cat, &b), hash);
    // Nessuna esecuzione registrata; una preparazione successiva riesce.
    let run = common::run_from(&mut cat, &vm, &b);
    assert_eq!(cat.read(|tx| tx.run(&run.run_id)).unwrap().vm_id, vm);
}

#[test]
fn interrupted_preparation_then_crash_is_reconciled_at_open() {
    // Processo ucciso durante il produttore: nessun codice di annullamento.
    // Simulato con un panico nella closure (che salta `abort`), poi il
    // catalogo viene rilasciato e riaperto.
    let dir = common::tempdir();
    let (vm, b) = {
        let mut cat = Catalog::open(dir.path()).unwrap();
        let vm = common::new_vm(&mut cat, "vm");
        let b = common::baseline(&mut cat, &vm, true);
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = cat.prepare_run_from_baseline(&vm, &b, |tmp, _| {
                common::write_bytes(tmp, b"QFI\xfb overlay parziale").unwrap();
                panic!("processo terminato durante qemu-img (simulato)");
            });
        }));
        assert!(r.is_err());
        // Il panico ha saltato l'annullamento: un intento resta aperto.
        assert_eq!(cat.open_intents().unwrap().len(), 1);
        (vm, b)
    };
    let mut cat = Catalog::open(dir.path()).unwrap();
    let rep = cat.open_report().unwrap().recovery.clone();
    assert_eq!(rep.intents.len(), 1, "{rep:?}");
    assert_eq!(rep.intents[0].outcome, RecoveryOutcome::RolledBack);
    assert_clean(dir.path(), &mut cat, &vm, &[b]);
}

#[cfg(windows)]
#[test]
fn unresolvable_intent_is_marked_needs_review_and_retried() {
    use std::os::windows::fs::OpenOptionsExt;

    let dir = common::tempdir();
    let (vm, b, temp) = {
        let mut cat = Catalog::open(dir.path()).unwrap();
        let vm = common::new_vm(&mut cat, "vm");
        let b = common::baseline(&mut cat, &vm, true);
        let mut temp = None;
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = cat.prepare_run_from_baseline(&vm, &b, |tmp, _| {
                common::write_bytes(tmp, b"QFI\xfb overlay").unwrap();
                temp = Some(tmp.to_path_buf());
                panic!("crash simulato");
            });
        }));
        assert!(r.is_err());
        (vm, b, temp.unwrap())
    };
    // Un altro processo tiene il temporaneo aperto in modo esclusivo: la
    // riconciliazione non può rimuoverlo.
    let holder = std::fs::OpenOptions::new()
        .read(true)
        .share_mode(0)
        .open(&temp)
        .unwrap();
    let mut cat = Catalog::open(dir.path()).expect("apertura nonostante l'intento irrisolto");
    let rep = cat.open_report().unwrap().recovery.clone();
    assert_eq!(rep.intents.len(), 1, "{rep:?}");
    assert_eq!(rep.intents[0].outcome, RecoveryOutcome::NeedsReview);
    assert!(rep.intents[0].detail.starts_with("needs_review:"));
    let id = rep.intents[0].intent_id;
    assert_eq!(cat.intent_state(id).unwrap(), IntentState::NeedsReview);
    assert_eq!(cat.intents_needing_review().unwrap(), vec![id]);
    assert_eq!(
        cat.read(|tx| tx.vm(&vm)).unwrap().lifecycle,
        Lifecycle::Recovering
    );
    // La VM resta bloccata finché l'intento non è risolto: `recovering`
    // (nessuna esecuzione) e occupata nel journal (nessun nuovo intento).
    let err = cat
        .prepare_run_from_baseline(&vm, &b, |t, _| common::write_bytes(t, b"x"))
        .unwrap_err();
    assert!(matches!(err, StorageError::StateConflict(_)), "{err:?}");
    let err = cat.promote_baseline(&req(&vm), |p| common::write_bytes(p, b"x"), None);
    assert!(
        matches!(err, Err(StorageError::StateConflict(_))),
        "{err:?}"
    );
    cat.with_tx(|tx| tx.set_vm_lifecycle(&vm, Lifecycle::Stopped))
        .unwrap();
    let err = cat
        .prepare_run_from_baseline(&vm, &b, |t, _| common::write_bytes(t, b"x"))
        .unwrap_err();
    assert!(matches!(err, StorageError::VmBusy(_)), "{err:?}");
    // Rimossa la causa, la riconciliazione successiva conclude l'intento.
    drop(holder);
    let rep = cat.recover().unwrap();
    assert_eq!(rep.intents.len(), 1);
    assert_eq!(rep.intents[0].prior_state, IntentState::NeedsReview);
    assert_eq!(rep.intents[0].outcome, RecoveryOutcome::RolledBack);
    assert!(cat.intents_needing_review().unwrap().is_empty());
    assert!(!temp.exists());
    assert_clean(dir.path(), &mut cat, &vm, &[b]);
    common::run_from(&mut cat, &vm, &b);
}

#[test]
fn crash_between_link_and_temp_removal_keeps_the_base_readonly() {
    // Pubblicazione con collegamento fisico: temporaneo e destinazione sono lo
    // stesso file finché il temporaneo non è rimosso. Crash in quel punto con
    // la riga già registrata: completamento in avanti, base in sola lettura.
    use vm_storage::{
        CatalogLayout, DiskId, DiskRecord, DiskRole, DiskStatus, IntentKind, IntentSpec,
        TargetKind, now_rfc3339,
    };
    let dir = common::tempdir();
    let (final_path, disk) = {
        let mut cat = Catalog::open(dir.path()).unwrap();
        let vm = common::new_vm(&mut cat, "vm");
        let b = BaselineId::new_v7();
        let disk = DiskId::new_v7();
        let rel = CatalogLayout::baseline_disk(&vm, &b);
        let i = cat
            .begin_intent(IntentSpec {
                kind: IntentKind::PromoteBaseline,
                vm_id: Some(vm),
                operation_id: None,
                target_kind: TargetKind::Disk,
                target_id: Some(disk.to_string()),
                final_path: Some(rel.clone()),
                read_only: true,
            })
            .unwrap();
        common::write_bytes(&i.temp_path, b"QFI\xfb base").unwrap();
        let final_path = i.final_path.clone().unwrap();
        std::fs::hard_link(&i.temp_path, &final_path).unwrap();
        cat.with_tx(|tx| {
            tx.insert_disk(&DiskRecord {
                disk_id: disk,
                vm_id: vm,
                role: DiskRole::Base,
                backing_disk_id: None,
                virtual_size_bytes: 1 << 30,
                allocated_bytes: 0,
                managed_path: rel.clone(),
                sha256: None,
                status: DiskStatus::Ready,
                created_at: now_rfc3339(),
            })
        })
        .unwrap();
        (final_path, disk)
    };
    let mut cat = Catalog::open(dir.path()).unwrap();
    let rep = cat.open_report().unwrap().recovery.clone();
    assert_eq!(rep.intents.len(), 1, "{rep:?}");
    assert_eq!(rep.intents[0].outcome, RecoveryOutcome::RolledForward);
    assert!(final_path.is_file());
    assert!(
        std::fs::metadata(&final_path)
            .unwrap()
            .permissions()
            .readonly(),
        "base di nuovo in sola lettura"
    );
    assert_eq!(cat.read(|tx| tx.disk(&disk)).unwrap().disk_id, disk);
    assert_eq!(
        std::fs::read_dir(dir.path().join("tmp")).unwrap().count(),
        0
    );
}

//! Interruption of composite operations through the high-level APIs: the
//! file producer (`qemu-img` in the product, a closure here) fails halfway
//! through a baseline promotion or a run preparation.
//! Complements `journal.rs`, which simulates crashes by building intents by
//! hand. At the end: reconciliation that cannot resolve an intent ("needs
//! review").

mod common;

use std::path::Path;

use vm_storage::{
    BaselineId, Catalog, IntentState, Lifecycle, PromoteBaseline, RecoveryOutcome, StorageError,
    VmId,
};

fn req(vm: &VmId) -> PromoteBaseline {
    PromoteBaseline {
        vm_id: *vm,
        label: "second baseline".into(),
        clean_shutdown: true,
        protected: false,
        virtual_size_bytes: 1 << 30,
    }
}

fn producer_failure() -> StorageError {
    StorageError::Unsupported("producer interrupted (simulated)")
}

/// No files under `vms/<vm>/base` other than the given baselines, `tmp/`
/// empty, no open intents, VM not busy.
fn assert_clean(dir: &Path, cat: &mut Catalog, vm: &VmId, baselines: &[BaselineId]) {
    assert!(cat.open_intents().unwrap().is_empty(), "no open intents");
    assert_eq!(
        std::fs::read_dir(dir.join("tmp")).unwrap().count(),
        0,
        "tmp/ empty"
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
    assert_eq!(found, want, "only the registered baselines on disk");
    let rows = cat.read(|tx| tx.baselines_of_vm(vm)).unwrap();
    assert_eq!(
        rows.len(),
        baselines.len(),
        "only the registered baselines in the DB"
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
                // Partial file, then interruption.
                common::write_bytes(p, b"QFI\xfb partial")?;
                Err(producer_failure())
            },
            None,
        )
        .unwrap_err();
    assert!(matches!(err, StorageError::Unsupported(_)), "{err:?}");
    assert_clean(dir.path(), &mut cat, &vm, &[]);
    // The VM does not stay busy: a subsequent promotion succeeds.
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
            |p| common::write_bytes(p, b"QFI\xfb complete second base"),
            Some(Box::new(|p: &Path| {
                common::write_bytes(p, b"OVMF partial")?;
                Err(producer_failure())
            })),
        )
        .unwrap_err();
    assert!(matches!(err, StorageError::Unsupported(_)), "{err:?}");
    // The second baseline's disk was already published (intent `applied`):
    // the rollback removes it together with the directory.
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
            common::write_bytes(tmp, b"QFI\xfb overlay partial")?;
            Err(producer_failure())
        })
        .unwrap_err();
    assert!(matches!(err, StorageError::Unsupported(_)), "{err:?}");
    assert_clean(dir.path(), &mut cat, &vm, &[b]);
    let overlays = dir.path().join("vms").join(vm.to_string()).join("overlays");
    let n = std::fs::read_dir(&overlays)
        .map(|it| it.count())
        .unwrap_or(0);
    assert_eq!(n, 0, "no overlay");
    let vm_rec = cat.read(|tx| tx.vm(&vm)).unwrap();
    assert_eq!(vm_rec.lifecycle, Lifecycle::Stopped);
    assert_eq!(common::base_hash(&mut cat, &b), hash);
    // No run recorded; a subsequent preparation succeeds.
    let run = common::run_from(&mut cat, &vm, &b);
    assert_eq!(cat.read(|tx| tx.run(&run.run_id)).unwrap().vm_id, vm);
}

#[test]
fn interrupted_preparation_then_crash_is_reconciled_at_open() {
    // Process killed during the producer: no rollback code runs.
    // Simulated with a panic in the closure (which skips `abort`), then the
    // catalog is released and reopened.
    let dir = common::tempdir();
    let (vm, b) = {
        let mut cat = Catalog::open(dir.path()).unwrap();
        let vm = common::new_vm(&mut cat, "vm");
        let b = common::baseline(&mut cat, &vm, true);
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = cat.prepare_run_from_baseline(&vm, &b, |tmp, _| {
                common::write_bytes(tmp, b"QFI\xfb overlay partial").unwrap();
                panic!("process terminated during qemu-img (simulated)");
            });
        }));
        assert!(r.is_err());
        // The panic skipped the rollback: one intent remains open.
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
                panic!("simulated crash");
            });
        }));
        assert!(r.is_err());
        (vm, b, temp.unwrap())
    };
    // Another process holds the temporary file open exclusively: the
    // reconciliation cannot remove it.
    let holder = std::fs::OpenOptions::new()
        .read(true)
        .share_mode(0)
        .open(&temp)
        .unwrap();
    let mut cat = Catalog::open(dir.path()).expect("open despite the unresolved intent");
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
    // The VM stays blocked until the intent is resolved: `recovering`
    // (no run) and busy in the journal (no new intent).
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
    // Once the cause is removed, the next reconciliation concludes the intent.
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
    // Publication via hard link: temporary and destination are the same file
    // until the temporary is removed. Crash at that point with the row
    // already recorded: roll forward, base read-only.
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
        "base read-only again"
    );
    assert_eq!(cat.read(|tx| tx.disk(&disk)).unwrap().disk_id, disk);
    assert_eq!(
        std::fs::read_dir(dir.path().join("tmp")).unwrap().count(),
        0
    );
}

//! Vincoli del catalogo: riferimenti, unicità, immutabilità, audit append-only.

mod common;

use rusqlite::params;
use vm_storage::{
    AuditEvent, AuditOutcome, Catalog, CatalogLayout, DiskRecord, DiskRole, DiskStatus, IntentKind,
    IntentSpec, Lifecycle, ManagedPath, MediaKind, MediaRecord, MediaSlot, MediaStatus, NewVm,
    RunState, StorageError, TargetKind, TerminationCause, now_rfc3339,
};
use vm_storage::{DiskId, MediaId};

#[test]
fn vm_name_is_unique_case_insensitive() {
    let dir = common::tempdir();
    let mut cat = Catalog::open(dir.path()).unwrap();
    common::new_vm(&mut cat, "Ubuntu Test");
    let err = cat
        .with_tx(|tx| {
            tx.insert_vm(&NewVm {
                name: "ubuntu test".into(),
                guest_os: "ubuntu".into(),
                config_version: 1,
                config: serde_json::json!({}),
            })
        })
        .unwrap_err();
    assert!(matches!(err, StorageError::Constraint(_)), "{err:?}");
}

#[test]
fn referenced_baseline_cannot_be_deleted() {
    let dir = common::tempdir();
    let mut cat = Catalog::open(dir.path()).unwrap();
    let vm = common::new_vm(&mut cat, "vm");
    let b = common::baseline(&mut cat, &vm, false);
    let before = common::base_hash(&mut cat, &b);
    let run = common::run_from(&mut cat, &vm, &b);

    // Trigger SQL.
    let err = cat.with_tx(|tx| tx.delete_baseline(&b)).unwrap_err();
    assert!(
        matches!(err, StorageError::Referenced("baseline_referenced")),
        "{err:?}"
    );
    // Controllo applicativo prima di toccare il filesystem.
    let err = cat
        .begin_intent(IntentSpec {
            kind: IntentKind::DeleteFile,
            vm_id: Some(vm),
            operation_id: None,
            target_kind: TargetKind::Baseline,
            target_id: Some(b.to_string()),
            final_path: Some(CatalogLayout::baseline_dir(&vm, &b)),
            read_only: false,
        })
        .unwrap_err();
    assert!(matches!(
        err,
        StorageError::Referenced("baseline_referenced")
    ));
    // Anche dopo la fine dell'esecuzione l'overlay conservato la referenzia.
    cat.with_tx(|tx| tx.end_run(&run.run_id, RunState::Stopped, TerminationCause::Shutdown))
        .unwrap();
    assert!(cat.with_tx(|tx| tx.delete_baseline(&b)).is_err());
    // Il disco base e l'overlay non sono cancellabili come righe isolate.
    let base_disk = cat.read(|tx| Ok(tx.baseline(&b)?.base_disk_id)).unwrap();
    let err = cat.with_tx(|tx| tx.delete_disk(&base_disk)).unwrap_err();
    assert!(matches!(err, StorageError::Referenced("disk_referenced")));
    let overlay = run.overlay_disk_id.unwrap();
    let err = cat.with_tx(|tx| tx.delete_disk(&overlay)).unwrap_err();
    assert!(matches!(err, StorageError::Referenced("disk_referenced")));

    assert_eq!(common::base_hash(&mut cat, &b), before, "base integra");
    assert!(
        dir.path()
            .join(CatalogLayout::baseline_disk(&vm, &b).as_str())
            .is_file()
    );
}

#[test]
fn protected_baseline_cannot_be_deleted() {
    let dir = common::tempdir();
    let mut cat = Catalog::open(dir.path()).unwrap();
    let vm = common::new_vm(&mut cat, "vm");
    let b = common::baseline(&mut cat, &vm, true);
    let err = cat.with_tx(|tx| tx.delete_baseline(&b)).unwrap_err();
    assert!(matches!(
        err,
        StorageError::Referenced("baseline_protected")
    ));
}

#[test]
fn unreferenced_baseline_is_deleted_through_the_journal() {
    let dir = common::tempdir();
    let mut cat = Catalog::open(dir.path()).unwrap();
    let vm = common::new_vm(&mut cat, "vm");
    let b = common::baseline(&mut cat, &vm, false);
    let i = cat
        .begin_intent(IntentSpec {
            kind: IntentKind::DeleteFile,
            vm_id: Some(vm),
            operation_id: None,
            target_kind: TargetKind::Baseline,
            target_id: Some(b.to_string()),
            final_path: Some(CatalogLayout::baseline_dir(&vm, &b)),
            read_only: false,
        })
        .unwrap();
    cat.mark_applied(&i).unwrap();
    assert!(i.temp_path.is_dir(), "directory spostata in tmp/");
    cat.commit(&i, |tx| tx.delete_baseline(&b)).unwrap();
    assert!(!i.temp_path.exists(), "temporaneo rimosso dopo la conferma");
    assert!(
        !dir.path()
            .join(CatalogLayout::baseline_dir(&vm, &b).as_str())
            .exists()
    );
    assert!(cat.read(|tx| tx.baseline(&b)).is_err());
}

#[test]
fn baseline_requires_powered_off_vm() {
    let dir = common::tempdir();
    let mut cat = Catalog::open(dir.path()).unwrap();
    let vm = common::new_vm(&mut cat, "vm");
    cat.with_tx(|tx| tx.set_vm_lifecycle(&vm, Lifecycle::Running))
        .unwrap();
    let err = cat
        .promote_baseline(
            &vm_storage::PromoteBaseline {
                vm_id: vm,
                label: "x".into(),
                clean_shutdown: true,
                protected: false,
                virtual_size_bytes: 1,
            },
            |p| common::write_bytes(p, b"x"),
            None,
        )
        .unwrap_err();
    assert!(matches!(err, StorageError::StateConflict(_)));
    assert!(cat.open_intents().unwrap().is_empty());
}

#[test]
fn referenced_media_cannot_be_deleted() {
    let dir = common::tempdir();
    let mut cat = Catalog::open(dir.path()).unwrap();
    let vm = common::new_vm(&mut cat, "vm");
    let media = MediaId::new_v7();
    cat.with_tx(|tx| {
        tx.insert_media(&MediaRecord {
            media_id: media,
            sha256: "a".repeat(64),
            size_bytes: 10,
            kind: MediaKind::Iso,
            declared_origin: "ubuntu.com, checksum verificato".into(),
            approved_at: now_rfc3339(),
            managed_path: CatalogLayout::media_file(&media, "iso")?,
            status: MediaStatus::Approved,
        })?;
        tx.attach_media(&vm, MediaSlot::Install, &media)
    })
    .unwrap();
    let err = cat.with_tx(|tx| tx.delete_media(&media)).unwrap_err();
    assert!(matches!(err, StorageError::Referenced("media_referenced")));
    cat.with_tx(|tx| {
        tx.detach_media(&vm, MediaSlot::Install)?;
        tx.delete_media(&media)
    })
    .unwrap();
}

#[test]
fn overlay_backing_must_be_a_base_and_base_is_immutable() {
    let dir = common::tempdir();
    let mut cat = Catalog::open(dir.path()).unwrap();
    let vm = common::new_vm(&mut cat, "vm");
    let primary = DiskId::new_v7();
    let rec = |id: DiskId, role, backing, path: &str| DiskRecord {
        disk_id: id,
        vm_id: vm,
        role,
        backing_disk_id: backing,
        virtual_size_bytes: 1024,
        allocated_bytes: 0,
        managed_path: ManagedPath::parse(path).unwrap(),
        sha256: None,
        status: DiskStatus::Ready,
        created_at: now_rfc3339(),
    };
    cat.with_tx(|tx| {
        tx.insert_disk(&rec(
            primary,
            DiskRole::Primary,
            None,
            "vms/a/primary.qcow2",
        ))
    })
    .unwrap();
    let err = cat
        .with_tx(|tx| {
            tx.insert_disk(&rec(
                DiskId::new_v7(),
                DiskRole::Overlay,
                Some(primary),
                "vms/a/overlays/x.qcow2",
            ))
        })
        .unwrap_err();
    assert!(
        matches!(&err, StorageError::Constraint(m) if m == "backing_not_base"),
        "{err:?}"
    );

    let b = common::baseline(&mut cat, &vm, false);
    let base = cat.read(|tx| Ok(tx.baseline(&b)?.base_disk_id)).unwrap();
    // La misura dei byte occupati resta ammessa; percorso e hash no.
    cat.with_tx(|tx| tx.set_disk_allocated(&base, 1)).unwrap();
    let dbp = dir.path().join(vm_storage::DB_FILE);
    drop(cat);
    let conn = vm_storage::migrate::open_connection(&dbp).unwrap();
    let e = conn
        .execute(
            "UPDATE disks SET sha256 = ?1 WHERE disk_id = ?2",
            params!["b".repeat(64), base.to_string()],
        )
        .unwrap_err();
    assert!(e.to_string().contains("base_immutable"), "{e}");
}

#[test]
fn audit_is_append_only_and_monotonic() {
    let dir = common::tempdir();
    let mut cat = Catalog::open(dir.path()).unwrap();
    let vm = common::new_vm(&mut cat, "vm");
    let mut last = 0;
    for (action, outcome) in [
        ("vm.create", AuditOutcome::Succeeded),
        ("vm.start", AuditOutcome::Denied),
        ("vm.stop", AuditOutcome::Succeeded),
    ] {
        let seq = cat
            .with_tx(|tx| {
                tx.append_audit(&AuditEvent {
                    action: action.into(),
                    outcome: Some(outcome),
                    vm_id: Some(vm),
                    error_code: (outcome == AuditOutcome::Denied)
                        .then(|| "permission_denied".into()),
                    ..AuditEvent::default()
                })
            })
            .unwrap();
        assert!(seq > last);
        last = seq;
    }
    let events = cat.read(|tx| tx.audit_since(0, 100)).unwrap();
    assert_eq!(events.len(), 3);
    assert_eq!(
        events[1].event.error_code.as_deref(),
        Some("permission_denied")
    );
    let dbp = dir.path().join(vm_storage::DB_FILE);
    drop(cat);
    let conn = vm_storage::migrate::open_connection(&dbp).unwrap();
    for sql in [
        "UPDATE audit SET outcome = 'allowed'",
        "DELETE FROM audit WHERE seq = 1",
    ] {
        let e = conn.execute(sql, []).unwrap_err();
        assert!(e.to_string().contains("audit_append_only"), "{e}");
    }
}

#[test]
fn config_revision_is_checked() {
    let dir = common::tempdir();
    let mut cat = Catalog::open(dir.path()).unwrap();
    let vm = common::new_vm(&mut cat, "vm");
    let cfg = serde_json::json!({"cpus": 4});
    let rev = cat
        .with_tx(|tx| tx.update_vm_config(&vm, 1, 1, &cfg))
        .unwrap();
    assert_eq!(rev, 2);
    let err = cat
        .with_tx(|tx| tx.update_vm_config(&vm, 1, 1, &cfg))
        .unwrap_err();
    assert!(matches!(err, StorageError::StateConflict(_)));
}

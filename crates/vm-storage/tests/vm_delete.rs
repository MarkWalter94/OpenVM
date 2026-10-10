//! Deletion of a VM with its rows and files through the journal (F2).

mod common;

use vm_storage::{
    Catalog, CatalogLayout, IntentKind, IntentSpec, IntentState, Lifecycle, ManagedPath,
    OperationId, PathRejection, RunState, StorageError, TargetKind, TerminationCause, VmId,
};

fn write(cat: &Catalog, rel: &str, bytes: &[u8]) {
    let rel = ManagedPath::parse(rel).unwrap();
    let abs = cat.layout().resolve(&rel).unwrap();
    std::fs::create_dir_all(abs.parent().unwrap()).unwrap();
    std::fs::write(abs, bytes).unwrap();
}

fn exists(cat: &Catalog, rel: &str) -> bool {
    let rel = ManagedPath::parse(rel).unwrap();
    std::fs::symlink_metadata(cat.layout().root().join(rel.as_str())).is_ok()
}

/// VM with a baseline, a closed run (overlay + vars) and unregistered files
/// written by the supervisor (primary disk, run log).
fn populated_vm(cat: &mut Catalog, name: &str) -> (VmId, String) {
    let vm = common::new_vm(cat, name);
    let b = common::baseline(cat, &vm, false);
    let run = common::run_from(cat, &vm, &b);
    cat.with_tx(|tx| tx.end_run(&run.run_id, RunState::Stopped, TerminationCause::Shutdown))
        .unwrap();
    write(cat, &format!("vms/{vm}/primary.qcow2"), b"QFI\xfb primary");
    write(cat, &format!("runs/{}/qemu.log", run.run_id), b"log");
    write(cat, &format!("runs/{}/vars.fd", run.run_id), b"vars");
    (vm, run.run_id.to_string())
}

#[test]
fn deletes_rows_files_and_directories() {
    let dir = common::tempdir();
    let mut cat = Catalog::open(dir.path()).unwrap();
    let (vm, run) = populated_vm(&mut cat, "to-delete");
    let (keep, _) = populated_vm(&mut cat, "to-keep");
    // Files kept by the supervisor under another identifier of the same VM.
    let alias = VmId::new_v7();
    write(&cat, &format!("vms/{alias}/primary.qcow2"), b"alias disk");
    let extra = [ManagedPath::parse(&format!("vms/{alias}")).unwrap()];

    let report = cat.delete_vm_with_files(&vm, &extra).unwrap();
    assert!(report.files_removed >= 6, "{report:?}");
    assert!(report.bytes_removed > 0);
    assert!(report.dirs_removed >= 3, "{report:?}");

    assert!(matches!(
        cat.read(|tx| tx.vm(&vm)),
        Err(StorageError::NotFound { .. })
    ));
    assert!(!exists(&cat, &format!("vms/{vm}")));
    assert!(!exists(&cat, &format!("vms/{alias}")));
    assert!(!exists(&cat, &format!("runs/{run}")));
    assert!(cat.open_intents().unwrap().is_empty());
    let conn = vm_storage::migrate::open_connection(&dir.path().join(vm_storage::DB_FILE)).unwrap();
    let n: i64 = conn
        .query_row(
            "SELECT (SELECT count(*) FROM runs WHERE vm_id = ?1)
                  + (SELECT count(*) FROM disks WHERE vm_id = ?1)
                  + (SELECT count(*) FROM baselines WHERE vm_id = ?1)
                  + (SELECT count(*) FROM firmware_state WHERE vm_id = ?1)
                  + (SELECT count(*) FROM operations WHERE vm_id = ?1)",
            [vm.to_string()],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(n, 0);
    drop(conn);
    // The other VM is untouched; `tmp/` is empty.
    assert!(cat.read(|tx| tx.vm(&keep)).is_ok());
    assert!(exists(&cat, &format!("vms/{keep}/primary.qcow2")));
    let tmp = cat.layout().root().join("tmp");
    assert_eq!(std::fs::read_dir(tmp).unwrap().count(), 0);
}

#[test]
fn vm_without_files_is_deleted() {
    let dir = common::tempdir();
    let mut cat = Catalog::open(dir.path()).unwrap();
    let vm = common::new_vm(&mut cat, "empty");
    let r = cat.delete_vm_with_files(&vm, &[]).unwrap();
    assert_eq!(r.files_removed, 0);
    assert!(cat.read(|tx| tx.vm(&vm)).is_err());
}

#[test]
fn running_vm_or_open_run_is_refused_without_effects() {
    let dir = common::tempdir();
    let mut cat = Catalog::open(dir.path()).unwrap();
    let (vm, _) = populated_vm(&mut cat, "busy");
    cat.with_tx(|tx| tx.set_vm_lifecycle(&vm, Lifecycle::Running))
        .unwrap();
    assert!(matches!(
        cat.delete_vm_with_files(&vm, &[]),
        Err(StorageError::StateConflict(_))
    ));
    assert!(exists(&cat, &format!("vms/{vm}/primary.qcow2")));

    // Stopped, but with a run still open.
    let vm2 = common::new_vm(&mut cat, "open-run");
    let b = common::baseline(&mut cat, &vm2, false);
    let _run = common::run_from(&mut cat, &vm2, &b);
    assert!(matches!(
        cat.delete_vm_with_files(&vm2, &[]),
        Err(StorageError::StateConflict(_))
    ));
    assert!(cat.read(|tx| tx.vm(&vm2)).is_ok());
    assert!(cat.open_intents().unwrap().is_empty());
}

#[test]
fn extra_directory_outside_the_vm_areas_is_refused() {
    let dir = common::tempdir();
    let mut cat = Catalog::open(dir.path()).unwrap();
    let vm = common::new_vm(&mut cat, "area");
    for bad in ["media", "vms", "tmp/x", "media/x"] {
        let p = ManagedPath::parse(bad).unwrap();
        assert!(
            matches!(
                cat.delete_vm_with_files(&vm, &[p]),
                Err(StorageError::InvalidPath {
                    reason: PathRejection::OutsideArea,
                    ..
                })
            ),
            "{bad}"
        );
    }
    assert!(cat.read(|tx| tx.vm(&vm)).is_ok());
}

#[test]
fn junction_inside_the_tree_aborts_before_any_change() {
    let dir = common::tempdir();
    let mut cat = Catalog::open(dir.path()).unwrap();
    let (vm, _) = populated_vm(&mut cat, "junction");
    let outside = common::tempdir();
    std::fs::write(outside.path().join("precious.txt"), b"keep").unwrap();
    let link = cat.layout().root().join(format!("vms/{vm}/link"));
    if !common::make_junction(&link, outside.path()) {
        eprintln!("junction not available: test skipped");
        return;
    }
    assert!(matches!(
        cat.delete_vm_with_files(&vm, &[]),
        Err(StorageError::InvalidPath {
            reason: PathRejection::Link,
            ..
        })
    ));
    assert!(outside.path().join("precious.txt").exists());
    assert!(exists(&cat, &format!("vms/{vm}/primary.qcow2")));
    assert!(cat.read(|tx| tx.vm(&vm)).is_ok());
}

fn vm_intent(cat: &mut Catalog, vm: &VmId, op: OperationId, rel: &str) -> vm_storage::Intent {
    cat.begin_intent(IntentSpec {
        kind: IntentKind::DeleteFile,
        vm_id: Some(*vm),
        operation_id: Some(op),
        target_kind: TargetKind::Vm,
        target_id: Some(vm.to_string()),
        final_path: Some(ManagedPath::parse(rel).unwrap()),
        read_only: false,
    })
    .unwrap()
}

#[test]
fn interrupted_after_a_move_rolls_the_whole_group_forward() {
    let dir = common::tempdir();
    let vm;
    {
        let mut cat = Catalog::open(dir.path()).unwrap();
        vm = common::new_vm(&mut cat, "crash-forward");
        write(&cat, &format!("vms/{vm}/a.bin"), b"a");
        write(&cat, &format!("vms/{vm}/b.bin"), b"b");
        let op = OperationId::new_v7();
        let a = vm_intent(&mut cat, &vm, op, &format!("vms/{vm}/a.bin"));
        let _b = vm_intent(&mut cat, &vm, op, &format!("vms/{vm}/b.bin"));
        cat.mark_applied(&a).unwrap();
        // Crash: the second file was never moved, no commit.
    }
    let cat = Catalog::open(dir.path()).unwrap();
    let rep = &cat.open_report().unwrap().recovery;
    assert_eq!(rep.intents.len(), 2, "{rep:?}");
    assert!(cat.open_intents().unwrap().is_empty());
    let mut cat = cat;
    assert!(cat.read(|tx| tx.vm(&vm)).is_err(), "VM rows deleted");
    assert!(!exists(&cat, &format!("vms/{vm}/a.bin")));
    assert!(!exists(&cat, &format!("vms/{vm}/b.bin")));
}

#[test]
fn interrupted_before_any_move_rolls_back() {
    let dir = common::tempdir();
    let vm;
    let ids;
    {
        let mut cat = Catalog::open(dir.path()).unwrap();
        vm = common::new_vm(&mut cat, "crash-back");
        write(&cat, &format!("vms/{vm}/a.bin"), b"a");
        write(&cat, &format!("vms/{vm}/b.bin"), b"b");
        let op = OperationId::new_v7();
        let a = vm_intent(&mut cat, &vm, op, &format!("vms/{vm}/a.bin"));
        let b = vm_intent(&mut cat, &vm, op, &format!("vms/{vm}/b.bin"));
        ids = [a.id, b.id];
    }
    let mut cat = Catalog::open(dir.path()).unwrap();
    for id in ids {
        assert_eq!(cat.intent_state(id).unwrap(), IntentState::Aborted);
    }
    let rec = cat.read(|tx| tx.vm(&vm)).unwrap();
    assert_eq!(rec.lifecycle, Lifecycle::Stopped, "not marked recovering");
    assert!(exists(&cat, &format!("vms/{vm}/a.bin")));
    assert!(exists(&cat, &format!("vms/{vm}/b.bin")));
    // The deletion can then be completed.
    cat.delete_vm_with_files(&vm, &[]).unwrap();
    assert!(!exists(&cat, &CatalogLayout::vm_dir(&vm).to_string()));
}

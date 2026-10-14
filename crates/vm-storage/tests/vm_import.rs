//! Journaled import of a VMware VM (ADR-0030, intent `import_vm`): the VM
//! row exists only with its complete disk; cancellation and a crash at any
//! step before the commit leave nothing behind (no temporary, no disk, no
//! VM directory, no row); a name conflict at the commit rolls back.

mod common;

use std::path::Path;

use vm_storage::{Catalog, IntentState, Lifecycle, NewVm, RecoveryOutcome, VmId};

fn new_vm(name: &str) -> NewVm {
    NewVm {
        name: name.into(),
        guest_os: "windows_vista".into(),
        config_version: 1,
        config: serde_json::json!({"imported": true}),
    }
}

fn vm_dir(root: &Path, vm: &VmId) -> std::path::PathBuf {
    root.join("vms").join(vm.to_string())
}

fn assert_nothing_left(root: &Path, cat: &mut Catalog, vm: &VmId) {
    assert!(cat.open_intents().unwrap().is_empty(), "no open intents");
    assert_eq!(
        std::fs::read_dir(root.join("tmp")).unwrap().count(),
        0,
        "tmp/ empty"
    );
    assert!(!vm_dir(root, vm).exists(), "no VM directory");
    assert!(cat.read(|tx| tx.vm_ids()).unwrap().is_empty(), "no VM row");
}

#[test]
fn import_publishes_the_disk_and_the_row_together() {
    let dir = common::tempdir();
    let mut cat = Catalog::open(dir.path()).unwrap();
    let vm = VmId::new_v7();
    let imp = cat.begin_vm_import(&vm, None).unwrap();
    assert!(imp.temp_path().starts_with(dir.path().join("tmp")));
    std::fs::write(imp.temp_path(), b"converted disk").unwrap();
    // The VM is busy for other intents while the import runs.
    assert!(cat.begin_vm_import(&vm, None).is_err());
    cat.finish_vm_import(imp, &new_vm("Vista"), Lifecycle::Stopped)
        .unwrap();
    let disk = vm_dir(dir.path(), &vm).join("primary.qcow2");
    assert_eq!(std::fs::read(&disk).unwrap(), b"converted disk");
    let row = cat.read(|tx| tx.vm(&vm)).unwrap();
    assert_eq!(row.name, "Vista");
    assert_eq!(row.lifecycle, Lifecycle::Stopped);
    assert!(cat.open_intents().unwrap().is_empty());
    assert_eq!(
        std::fs::read_dir(dir.path().join("tmp")).unwrap().count(),
        0
    );
}

#[test]
fn cancelled_import_leaves_nothing() {
    let dir = common::tempdir();
    let mut cat = Catalog::open(dir.path()).unwrap();
    let vm = VmId::new_v7();
    let imp = cat.begin_vm_import(&vm, None).unwrap();
    std::fs::write(imp.temp_path(), b"partial").unwrap();
    let id = imp.intent_id();
    cat.abort_vm_import(imp).unwrap();
    assert_eq!(cat.intent_state(id).unwrap(), IntentState::Aborted);
    assert_nothing_left(dir.path(), &mut cat, &vm);
}

#[test]
fn crash_during_the_conversion_is_rolled_back_at_open() {
    let dir = common::tempdir();
    let vm = VmId::new_v7();
    let id = {
        let mut cat = Catalog::open(dir.path()).unwrap();
        let imp = cat.begin_vm_import(&vm, None).unwrap();
        // qemu-img wrote part of the disk, then the supervisor died.
        std::fs::write(imp.temp_path(), vec![7u8; 4096]).unwrap();
        imp.intent_id()
    };
    let mut cat = Catalog::open(dir.path()).unwrap();
    let rep = &cat.open_report().unwrap().recovery;
    let r = rep.intents.iter().find(|r| r.intent_id == id).unwrap();
    assert_eq!(r.outcome, RecoveryOutcome::RolledBack, "{r:?}");
    assert_eq!(cat.intent_state(id).unwrap(), IntentState::Aborted);
    assert_nothing_left(dir.path(), &mut cat, &vm);
}

#[test]
fn crash_after_publishing_before_the_row_is_rolled_back_at_open() {
    let dir = common::tempdir();
    let vm = VmId::new_v7();
    {
        let mut cat = Catalog::open(dir.path()).unwrap();
        let imp = cat.begin_vm_import(&vm, None).unwrap();
        std::fs::write(imp.temp_path(), b"complete disk").unwrap();
        // Published by hand as `mark_applied` does, then the crash.
        let fin = imp.final_path().unwrap().to_path_buf();
        std::fs::hard_link(imp.temp_path(), &fin).unwrap();
        assert!(fin.exists());
    }
    let mut cat = Catalog::open(dir.path()).unwrap();
    assert_nothing_left(dir.path(), &mut cat, &vm);
}

#[test]
fn name_conflict_at_the_commit_rolls_back() {
    let dir = common::tempdir();
    let mut cat = Catalog::open(dir.path()).unwrap();
    common::new_vm(&mut cat, "Taken");
    let vm = VmId::new_v7();
    let imp = cat.begin_vm_import(&vm, None).unwrap();
    std::fs::write(imp.temp_path(), b"disk").unwrap();
    assert!(
        cat.finish_vm_import(imp, &new_vm("taken"), Lifecycle::Stopped)
            .is_err()
    );
    assert!(cat.open_intents().unwrap().is_empty());
    assert_eq!(
        std::fs::read_dir(dir.path().join("tmp")).unwrap().count(),
        0
    );
    assert!(!vm_dir(dir.path(), &vm).exists());
    assert_eq!(cat.read(|tx| tx.vm_ids()).unwrap().len(), 1);
}

#[test]
fn existing_destination_or_row_refuses_the_import() {
    let dir = common::tempdir();
    let mut cat = Catalog::open(dir.path()).unwrap();
    let existing = common::new_vm(&mut cat, "Exists");
    assert!(cat.begin_vm_import(&existing, None).is_err(), "row present");
    let vm = VmId::new_v7();
    std::fs::create_dir_all(vm_dir(dir.path(), &vm)).unwrap();
    std::fs::write(vm_dir(dir.path(), &vm).join("primary.qcow2"), b"foreign").unwrap();
    assert!(cat.begin_vm_import(&vm, None).is_err(), "file present");
    assert_eq!(
        std::fs::read(vm_dir(dir.path(), &vm).join("primary.qcow2")).unwrap(),
        b"foreign",
        "never overwritten"
    );
}

//! Hostile paths, junctions, catalog lock and cautious cleanup of `tmp/`.

mod common;

use std::time::{Duration, SystemTime};

use vm_storage::{
    Catalog, CatalogLayout, IntentKind, IntentSpec, ManagedPath, PathRejection, StorageError,
    TargetKind, UtcMillis,
};
use vm_storage::{DiskId, RunId};

fn rejection(err: StorageError) -> PathRejection {
    match err {
        StorageError::InvalidPath { reason, .. } => reason,
        other => panic!("unexpected error: {other:?}"),
    }
}

#[test]
fn hostile_managed_paths_are_rejected_before_any_access() {
    for raw in [
        "C:\\Windows\\System32\\config\\SAM",
        "\\\\server\\share\\disk.qcow2",
        "\\\\?\\GLOBALROOT\\Device\\HarddiskVolume1",
        "\\\\.\\PhysicalDrive0",
        "../../etc/passwd",
        "vms/../../outside",
        "media/x.iso:Zone.Identifier",
        "media/CON",
        "/abs/path",
    ] {
        assert!(ManagedPath::parse(raw).is_err(), "{raw:?} accepted");
    }
}

#[test]
fn intent_destination_in_tmp_is_refused() {
    let dir = common::tempdir();
    let mut cat = Catalog::open(dir.path()).unwrap();
    let vm = common::new_vm(&mut cat, "vm");
    let err = cat
        .begin_intent(IntentSpec {
            kind: IntentKind::CreateDisk,
            vm_id: Some(vm),
            operation_id: None,
            target_kind: TargetKind::Disk,
            target_id: Some(DiskId::new_v7().to_string()),
            final_path: Some(ManagedPath::parse("tmp/x.qcow2").unwrap()),
            read_only: false,
        })
        .unwrap_err();
    assert_eq!(rejection(err), PathRejection::OutsideArea);
}

#[test]
fn existing_destination_is_never_overwritten() {
    let dir = common::tempdir();
    let mut cat = Catalog::open(dir.path()).unwrap();
    let vm = common::new_vm(&mut cat, "vm");
    let b = common::baseline(&mut cat, &vm, false);
    // Attempt to create a file over the existing base.
    let err = cat
        .begin_intent(IntentSpec {
            kind: IntentKind::CreateOverlay,
            vm_id: Some(vm),
            operation_id: None,
            target_kind: TargetKind::Disk,
            target_id: Some(DiskId::new_v7().to_string()),
            final_path: Some(CatalogLayout::baseline_disk(&vm, &b)),
            read_only: false,
        })
        .unwrap_err();
    assert!(matches!(err, StorageError::StateConflict(_)), "{err:?}");
}

#[test]
fn junction_inside_catalog_is_refused() {
    let dir = common::tempdir();
    let outside = common::tempdir();
    let mut cat = Catalog::open(dir.path()).unwrap();
    let vm = common::new_vm(&mut cat, "vm");
    let vm_dir = cat
        .layout()
        .ensure_dir(&CatalogLayout::vm_dir(&vm))
        .unwrap();
    let link = vm_dir.join("overlays");
    if !common::make_junction(&link, outside.path()) {
        eprintln!("junction not available: test skipped");
        return;
    }
    let run = RunId::new_v7();
    let err = cat
        .begin_intent(IntentSpec {
            kind: IntentKind::CreateOverlay,
            vm_id: Some(vm),
            operation_id: None,
            target_kind: TargetKind::Disk,
            target_id: Some(DiskId::new_v7().to_string()),
            final_path: Some(CatalogLayout::overlay(&vm, &run)),
            read_only: false,
        })
        .unwrap_err();
    assert_eq!(rejection(err), PathRejection::Link);
    let err = cat
        .layout()
        .resolve(&CatalogLayout::overlay(&vm, &run))
        .unwrap_err();
    assert_eq!(rejection(err), PathRejection::Link);
    let err = cat
        .layout()
        .ensure_dir(&ManagedPath::parse(&format!("vms/{vm}/overlays/sub")).unwrap())
        .unwrap_err();
    assert_eq!(rejection(err), PathRejection::Link);
    assert_eq!(
        std::fs::read_dir(outside.path()).unwrap().count(),
        0,
        "nothing written outside the catalog"
    );
    std::fs::remove_dir(&link).unwrap();
}

#[test]
fn catalog_root_that_is_a_junction_is_refused() {
    let base = common::tempdir();
    let real = base.path().join("real");
    std::fs::create_dir(&real).unwrap();
    let link = base.path().join("link");
    if !common::make_junction(&link, &real) {
        eprintln!("junction not available: test skipped");
        return;
    }
    let err = Catalog::open(&link).unwrap_err();
    assert_eq!(rejection(err), PathRejection::Link);
    std::fs::remove_dir(&link).unwrap();
}

#[test]
fn symlink_file_in_managed_path_is_refused() {
    let dir = common::tempdir();
    let cat = Catalog::open(dir.path()).unwrap();
    let target = dir.path().join("outside.bin");
    std::fs::write(&target, b"x").unwrap();
    let link = dir.path().join("media").join("evil.iso");
    #[cfg(windows)]
    let made = std::os::windows::fs::symlink_file(&target, &link).is_ok();
    #[cfg(unix)]
    let made = std::os::unix::fs::symlink(&target, &link).is_ok();
    if !made {
        eprintln!("symlink not allowed (missing privilege): test skipped");
        return;
    }
    let err = cat
        .layout()
        .resolve(&ManagedPath::parse("media/evil.iso").unwrap())
        .unwrap_err();
    assert_eq!(rejection(err), PathRejection::Link);
}

#[test]
fn second_open_of_same_catalog_is_refused() {
    let dir = common::tempdir();
    let first = Catalog::open(dir.path()).unwrap();
    match Catalog::open(dir.path()) {
        Err(e @ StorageError::CatalogLocked(_)) => {
            assert_eq!(e.public_code(), Some("vm_busy"));
        }
        other => panic!("second instance accepted: {other:?}"),
    }
    // From another thread too.
    let p = dir.path().to_path_buf();
    let res = std::thread::spawn(move || Catalog::open(&p).map(|_| ()))
        .join()
        .unwrap();
    assert!(matches!(res, Err(StorageError::CatalogLocked(_))));
    drop(first);
    Catalog::open(dir.path()).expect("lock released on close");
}

#[test]
fn tmp_cleanup_is_prudent() {
    let dir = common::tempdir();
    let cat = Catalog::open(dir.path()).unwrap();
    let tmp = dir.path().join("tmp");
    let old = SystemTime::now() - Duration::from_secs(3600);
    let set_old = |p: &std::path::Path| {
        std::fs::File::options()
            .write(true)
            .open(p)
            .unwrap()
            .set_modified(old)
            .unwrap();
    };
    std::fs::write(tmp.join("old.part"), b"x").unwrap();
    set_old(&tmp.join("old.part"));
    std::fs::write(tmp.join("recent.part"), b"x").unwrap();
    std::fs::create_dir(tmp.join("subdir")).unwrap();
    std::fs::write(tmp.join("subdir").join("inner.part"), b"x").unwrap();
    set_old(&tmp.join("subdir").join("inner.part"));
    // Old file outside tmp/: never touched.
    std::fs::write(dir.path().join("vms").join("keep.txt"), b"x").unwrap();
    set_old(&dir.path().join("vms").join("keep.txt"));
    let outside = common::tempdir();
    let victim = outside.path().join("victim.txt");
    std::fs::write(&victim, b"x").unwrap();
    set_old(&victim);
    let junction = common::make_junction(&tmp.join("jlink"), outside.path());

    // "Previous" start halfway between: the old file predates it, the recent one does not.
    let start = UtcMillis::from_system_time(SystemTime::now() - Duration::from_secs(60));
    let rep = cat.layout().clean_tmp(start, &[]).unwrap();
    assert_eq!(rep.removed, vec!["old.part".to_owned()]);
    assert_eq!(rep.kept_recent, vec!["recent.part".to_owned()]);
    assert!(rep.skipped.contains(&"subdir".to_owned()));
    assert!(tmp.join("subdir").join("inner.part").exists());
    assert!(dir.path().join("vms").join("keep.txt").exists());
    assert!(victim.exists(), "no access through the junction");
    if junction {
        assert!(rep.skipped.contains(&"jlink".to_owned()));
        std::fs::remove_dir(tmp.join("jlink")).unwrap();
    }
    drop(cat);

    // On reopen the "recent" file predates the new start and is removed.
    let cat = Catalog::open(dir.path()).unwrap();
    let rep = &cat.open_report().unwrap().tmp_cleanup;
    assert!(rep.removed.contains(&"recent.part".to_owned()), "{rep:?}");
    assert!(dir.path().join("vms").join("keep.txt").exists());
}

#[test]
fn absolute_paths_are_accepted_only_inside_the_root() {
    let dir = common::tempdir();
    let outside = common::tempdir();
    let cat = Catalog::open(dir.path()).unwrap();
    let layout = cat.layout();
    let ok = layout
        .managed_from_absolute(&dir.path().join("media").join("a.iso"))
        .unwrap();
    assert_eq!(ok.as_str(), "media/a.iso");
    let cases: Vec<(std::path::PathBuf, PathRejection)> = vec![
        (outside.path().join("x.iso"), PathRejection::OutsideArea),
        (
            std::path::PathBuf::from("media/a.iso"),
            PathRejection::OutsideArea,
        ),
        (
            dir.path().join("media").join("..").join("..").join("x"),
            PathRejection::Traversal,
        ),
        (dir.path().to_path_buf(), PathRejection::Empty),
        (
            dir.path().join("media").join("CON"),
            PathRejection::ReservedName,
        ),
    ];
    for (p, want) in cases {
        let err = layout.managed_from_absolute(&p).unwrap_err();
        assert_eq!(rejection(err), want, "{}", p.display());
    }
    // Junction under the root pointing outside: refused even though the
    // textual prefix is inside the root.
    let link = dir.path().join("runs").join("jl");
    if common::make_junction(&link, outside.path()) {
        let err = layout
            .managed_from_absolute(&link.join("x.log"))
            .unwrap_err();
        assert_eq!(rejection(err), PathRejection::Link);
        std::fs::remove_dir(&link).unwrap();
    }
}

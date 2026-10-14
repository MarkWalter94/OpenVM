//! Move of a whole catalog to another folder (Settings → Move catalog…):
//! copy verified file by file, destination opened before the source goes,
//! source untouched on every failure or cancellation.

mod common;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use sha2::{Digest, Sha256};
use vm_storage::{
    Catalog, MediaKind, MoveFault, MoveMethod, MovePhase, MoveProgress, MoveReport, MoveRequest,
    PathRejection, StorageError, VmId, is_move_cancelled, move_catalog,
};

/// Plenty of free space.
fn roomy(_: &Path) -> std::io::Result<u64> {
    Ok(u64::MAX)
}

fn never() -> bool {
    false
}

/// Catalog with a VM, a baseline (read-only base), a medium with a managed
/// copy as older catalogs have (read-only, larger than one copy block) and
/// a few unregistered files.
fn populated(root: &Path) -> VmId {
    let mut cat = Catalog::open(root).unwrap();
    let vm = common::new_vm(&mut cat, "moved");
    common::baseline(&mut cat, &vm, false);
    common::managed_media(
        &mut cat,
        &b"CD001 fake iso ".repeat(150_000),
        MediaKind::Iso,
        "test image",
    );
    let primary = root.join("vms").join(vm.to_string()).join("primary.qcow2");
    std::fs::write(primary, b"QFI\xfb primary").unwrap();
    std::fs::create_dir_all(root.join("sessions").join("s1")).unwrap();
    std::fs::write(root.join("sessions").join("s1").join("report.json"), b"{}").unwrap();
    vm
}

/// Relative path → (SHA-256, read-only) of every file under `root`,
/// except the lock file and the database: every opening of a catalog (the
/// move's own included, as at a supervisor start) records its start there.
fn snapshot(root: &Path) -> BTreeMap<PathBuf, (String, bool)> {
    fn walk(root: &Path, dir: &Path, out: &mut BTreeMap<PathBuf, (String, bool)>) {
        for e in std::fs::read_dir(dir).unwrap() {
            let p = e.unwrap().path();
            let m = std::fs::symlink_metadata(&p).unwrap();
            let name = p.file_name().unwrap().to_string_lossy().into_owned();
            if m.is_dir() {
                walk(root, &p, out);
            } else if name != vm_storage::LOCK_FILE && !name.starts_with(vm_storage::DB_FILE) {
                let rel = p.strip_prefix(root).unwrap().to_path_buf();
                let hash: String = Sha256::digest(std::fs::read(&p).unwrap())
                    .iter()
                    .map(|b| format!("{b:02x}"))
                    .collect();
                out.insert(rel, (hash, m.permissions().readonly()));
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(root, root, &mut out);
    out
}

fn request(from: &Path, to: &Path) -> MoveRequest {
    MoveRequest {
        from: from.to_path_buf(),
        to: to.to_path_buf(),
        ..MoveRequest::default()
    }
}

fn run(req: &MoveRequest) -> vm_storage::Result<MoveReport> {
    move_catalog(req, &roomy, &|_| {}, &never)
}

fn opens_with_vm(root: &Path, vm: &VmId) {
    let mut cat = Catalog::open(root).unwrap();
    assert_eq!(cat.read(|tx| tx.vm(vm)).unwrap().vm_id, *vm);
}

#[test]
fn copy_moves_every_file_verified_and_removes_the_source() {
    let base = common::tempdir();
    let from = base.path().join("catalog");
    let to = base.path().join("moved");
    let vm = populated(&from);
    let before = snapshot(&from);
    let phases = std::sync::Mutex::new(Vec::new());
    let report = move_catalog(
        &MoveRequest {
            fault: Some(MoveFault::NoRename),
            ..request(&from, &to)
        },
        &roomy,
        &|p: MoveProgress| phases.lock().unwrap().push(p),
        &never,
    )
    .unwrap();

    assert_eq!(report.method, MoveMethod::Copied);
    assert!(report.source_removed, "{report:?}");
    assert_eq!(report.source_cleanup_error, None);
    assert_eq!((report.vms, report.media, report.baselines), (1, 1, 1));
    assert!(report.files >= 6 && report.bytes > 2_000_000, "{report:?}");
    assert!(!from.exists(), "source left behind");
    // Same files, same bytes, same read-only attributes (base and medium).
    let after = snapshot(&to);
    assert_eq!(after, before);
    assert!(after.values().filter(|(_, ro)| *ro).count() >= 2);
    opens_with_vm(&to, &vm);

    let phases = phases.into_inner().unwrap();
    let seen: Vec<MovePhase> = phases.iter().map(|p| p.phase).collect();
    for want in [
        MovePhase::Checking,
        MovePhase::Copying,
        MovePhase::Verifying,
        MovePhase::Validating,
        MovePhase::RemovingSource,
    ] {
        assert!(seen.contains(&want), "{want:?} not reported: {seen:?}");
    }
    let last_copy = phases
        .iter()
        .rfind(|p| p.phase == MovePhase::Copying)
        .unwrap();
    assert_eq!(last_copy.done_bytes, report.bytes);
    assert_eq!(last_copy.total_bytes, report.bytes);
}

/// ADR-0029: a medium used in place is outside the catalog: the move
/// neither copies nor touches the user's file, and the moved catalog keeps
/// the reference (same row, same path, still verifiable).
#[test]
fn media_used_in_place_keep_their_reference_and_are_not_copied() {
    let base = common::tempdir();
    let from = base.path().join("catalog");
    let to = base.path().join("moved");
    let isos = common::tempdir();
    let iso = isos.path().join("user.iso");
    let content = b"CD001 user's iso ".repeat(100_000);
    std::fs::write(&iso, &content).unwrap();
    let m = {
        let mut cat = Catalog::open(&from).unwrap();
        common::new_vm(&mut cat, "with media");
        cat.approve_media(&iso, MediaKind::Iso, "mine").unwrap()
    };
    let report = move_catalog(
        &MoveRequest {
            fault: Some(MoveFault::NoRename),
            ..request(&from, &to)
        },
        &roomy,
        &|_| {},
        &never,
    )
    .unwrap();
    assert_eq!(report.media, 1);
    assert!(
        report.bytes < content.len() as u64,
        "the ISO was copied: {report:?}"
    );
    assert!(
        std::fs::read_dir(to.join("media")).map_or(0, Iterator::count) == 0,
        "no copy under media/"
    );
    assert_eq!(
        std::fs::read(&iso).unwrap(),
        content,
        "user's file untouched"
    );
    let mut cat = Catalog::open(&to).unwrap();
    let moved = cat.read(|tx| tx.media(&m.media_id)).unwrap();
    assert_eq!(moved, m);
    cat.verify_media(&m.media_id).unwrap();
}
#[test]
fn same_volume_move_renames_the_folder() {
    let base = common::tempdir();
    let from = base.path().join("catalog");
    let to = base.path().join("elsewhere");
    let vm = populated(&from);
    let report = run(&request(&from, &to)).unwrap();
    assert_eq!(report.method, MoveMethod::Renamed);
    assert!(report.source_removed);
    assert!(!from.exists());
    opens_with_vm(&to, &vm);
}

#[test]
fn keep_source_copies_and_leaves_both_catalogs_usable() {
    let base = common::tempdir();
    let from = base.path().join("catalog");
    let to = base.path().join("copy");
    let vm = populated(&from);
    let report = run(&MoveRequest {
        keep_source: true,
        ..request(&from, &to)
    })
    .unwrap();
    assert_eq!(report.method, MoveMethod::Copied);
    assert!(!report.source_removed);
    opens_with_vm(&from, &vm);
    opens_with_vm(&to, &vm);
}

#[test]
fn existing_empty_destination_folder_is_used() {
    let base = common::tempdir();
    let from = base.path().join("catalog");
    let to = base.path().join("empty");
    std::fs::create_dir(&to).unwrap();
    let vm = populated(&from);
    for fault in [Some(MoveFault::NoRename), None] {
        let report = run(&MoveRequest {
            fault,
            ..request(&from, &to)
        })
        .unwrap();
        assert!(report.source_removed);
        opens_with_vm(&to, &vm);
        // And back again, through the other path.
        std::fs::create_dir(&from).unwrap();
        run(&MoveRequest {
            fault,
            ..request(&to, &from)
        })
        .unwrap();
        std::fs::create_dir(&to).unwrap();
    }
    opens_with_vm(&from, &vm);
}

/// Failure partway, altered copy, cancellation and lack of space: the
/// destination disappears, the source is byte for byte what it was and
/// still opens.
#[test]
fn failures_and_cancel_leave_the_source_untouched() {
    let base = common::tempdir();
    let from = base.path().join("catalog");
    let to = base.path().join("moved");
    let vm = populated(&from);
    let before = snapshot(&from);

    let failing = MoveRequest {
        fault: Some(MoveFault::FailAfterFiles(3)),
        ..request(&from, &to)
    };
    let e = run(&failing).unwrap_err();
    assert!(e.to_string().contains("injected failure"), "{e}");
    assert!(!to.exists(), "partial destination left");

    let altered = MoveRequest {
        fault: Some(MoveFault::AlterCopy(1)),
        ..request(&from, &to)
    };
    let e = run(&altered).unwrap_err();
    assert!(matches!(e, StorageError::IntegrityMismatch { .. }), "{e}");
    assert!(!to.exists(), "altered destination left");

    // Cancelled in the middle of the copy (the medium spans several
    // blocks).
    let polls = AtomicUsize::new(0);
    let copying = std::sync::atomic::AtomicBool::new(false);
    let e = move_catalog(
        &MoveRequest {
            fault: Some(MoveFault::NoRename),
            ..request(&from, &to)
        },
        &roomy,
        &|p: MoveProgress| {
            if p.phase == MovePhase::Copying && p.done_bytes > 0 {
                copying.store(true, Ordering::SeqCst);
            }
        },
        &|| copying.load(Ordering::SeqCst) && polls.fetch_add(1, Ordering::SeqCst) > 0,
    )
    .unwrap_err();
    assert!(is_move_cancelled(&e), "{e}");
    assert!(!to.exists(), "cancelled destination left");

    let e = move_catalog(
        &MoveRequest {
            fault: Some(MoveFault::NoRename),
            ..request(&from, &to)
        },
        &|_| Ok(1 << 20),
        &|_| {},
        &never,
    )
    .unwrap_err();
    assert!(matches!(e, StorageError::ResourceLimit(_)), "{e}");
    assert!(!to.exists());

    assert_eq!(snapshot(&from), before);
    opens_with_vm(&from, &vm);
}

/// A destination folder chosen by the user stays (empty) after a failure.
#[test]
fn failure_keeps_an_existing_empty_destination_folder() {
    let base = common::tempdir();
    let from = base.path().join("catalog");
    let to = base.path().join("chosen");
    std::fs::create_dir(&to).unwrap();
    populated(&from);
    run(&MoveRequest {
        fault: Some(MoveFault::FailAfterFiles(2)),
        ..request(&from, &to)
    })
    .unwrap_err();
    assert!(to.is_dir());
    assert_eq!(std::fs::read_dir(&to).unwrap().count(), 0);
}

#[test]
fn non_empty_destination_is_refused() {
    let base = common::tempdir();
    let from = base.path().join("catalog");
    let to = base.path().join("busy");
    std::fs::create_dir(&to).unwrap();
    std::fs::write(to.join("notes.txt"), b"mine").unwrap();
    populated(&from);
    let before = snapshot(&from);
    let e = run(&request(&from, &to)).unwrap_err();
    assert!(e.to_string().contains("not empty"), "{e}");
    assert_eq!(std::fs::read(to.join("notes.txt")).unwrap(), b"mine");
    assert_eq!(std::fs::read_dir(&to).unwrap().count(), 1);
    assert_eq!(snapshot(&from), before);

    // A file as destination.
    let e = run(&request(&from, &to.join("notes.txt"))).unwrap_err();
    assert!(matches!(e, StorageError::StateConflict(_)), "{e}");
}

#[test]
fn destination_inside_the_source_or_around_it_is_refused() {
    let base = common::tempdir();
    let from = base.path().join("catalog");
    populated(&from);
    let before = snapshot(&from);
    let inside = from.join("vms").join("nested");
    let mut cases = vec![
        inside.clone(),
        from.join("new"),
        from.clone(),
        base.path().to_path_buf(),
    ];
    // Same folder spelled with another case (case-insensitive volumes).
    cases.push(base.path().join("CATALOG").join("x"));
    for to in cases {
        let e = run(&request(&from, &to)).unwrap_err();
        assert!(
            matches!(e, StorageError::StateConflict(ref m) if m.contains("outside the catalog")),
            "{}: {e}",
            to.display()
        );
    }
    assert!(!inside.exists());
    assert_eq!(snapshot(&from), before);
}

#[test]
fn relative_or_traversing_paths_are_refused() {
    let base = common::tempdir();
    let from = base.path().join("catalog");
    populated(&from);
    let e = run(&request(&from, Path::new("relative"))).unwrap_err();
    assert!(matches!(
        e,
        StorageError::InvalidPath {
            reason: PathRejection::Absolute,
            ..
        }
    ));
    let e = run(&request(&from, &base.path().join("a").join("..").join("b"))).unwrap_err();
    assert!(matches!(
        e,
        StorageError::InvalidPath {
            reason: PathRejection::Traversal,
            ..
        }
    ));
    let e = run(&request(
        &base.path().join("x").join("y").join("z"),
        &base.path().join("w"),
    ))
    .unwrap_err();
    assert!(matches!(e, StorageError::Io { .. }), "{e}");
}

#[test]
fn folder_without_a_catalog_is_not_moved() {
    let base = common::tempdir();
    let from = base.path().join("plain");
    std::fs::create_dir(&from).unwrap();
    std::fs::write(from.join("file.txt"), b"x").unwrap();
    let e = run(&request(&from, &base.path().join("to"))).unwrap_err();
    assert!(
        matches!(
            e,
            StorageError::NotFound {
                kind: "catalog",
                ..
            }
        ),
        "{e}"
    );
    assert!(!base.path().join("to").exists());
    // Nothing was created in the folder (no new catalog).
    assert_eq!(std::fs::read_dir(&from).unwrap().count(), 1);
}

#[test]
fn missing_destination_parent_is_refused() {
    let base = common::tempdir();
    let from = base.path().join("catalog");
    populated(&from);
    let e = run(&request(&from, &base.path().join("no").join("such"))).unwrap_err();
    assert!(matches!(e, StorageError::NotFound { .. }), "{e}");
}

#[test]
fn catalog_in_use_or_with_a_running_vm_is_not_moved() {
    let base = common::tempdir();
    let from = base.path().join("catalog");
    let to = base.path().join("moved");
    let vm = populated(&from);
    {
        let _open = Catalog::open(&from).unwrap();
        let e = run(&request(&from, &to)).unwrap_err();
        assert!(matches!(e, StorageError::CatalogLocked(_)), "{e}");
    }
    {
        let mut cat = Catalog::open(&from).unwrap();
        let b = cat
            .read(|tx| Ok(tx.baselines_of_vm(&vm)?[0].baseline_id))
            .unwrap();
        common::run_from(&mut cat, &vm, &b);
    }
    let e = run(&request(&from, &to)).unwrap_err();
    assert!(e.to_string().contains("have not ended"), "{e}");
    assert!(!to.exists());
}

/// Runtime files of the supervisor named by the caller are not copied and
/// are reported; anything else unexpected is copied like the rest.
#[test]
fn named_runtime_entries_stay_behind_and_are_reported() {
    let base = common::tempdir();
    let from = base.path().join("catalog");
    let to = base.path().join("moved");
    populated(&from);
    std::fs::write(from.join("emergency.endpoint"), b"pipe").unwrap();
    std::fs::create_dir_all(from.join("p").join("d")).unwrap();
    std::fs::write(from.join("unknown.txt"), b"kept").unwrap();
    let report = run(&MoveRequest {
        leave_behind: vec!["emergency.endpoint".into(), "p".into(), ".run".into()],
        fault: Some(MoveFault::NoRename),
        ..request(&from, &to)
    })
    .unwrap();
    assert_eq!(report.left_behind, vec!["emergency.endpoint", "p"]);
    assert!(!to.join("emergency.endpoint").exists());
    assert!(!to.join("p").exists());
    assert_eq!(std::fs::read(to.join("unknown.txt")).unwrap(), b"kept");
    assert!(!from.exists());
}

/// A junction inside the catalog is refused like everywhere else in
/// storage: nothing is copied (Windows; elsewhere a symbolic link).
#[test]
fn links_inside_the_catalog_are_refused() {
    let base = common::tempdir();
    let from = base.path().join("catalog");
    let to = base.path().join("moved");
    let outside = common::tempdir();
    populated(&from);
    let link = from.join("vms").join("link");
    #[cfg(unix)]
    std::os::unix::fs::symlink(outside.path(), &link).unwrap();
    #[cfg(windows)]
    if !common::make_junction(&link, outside.path()) {
        return;
    }
    for fault in [None, Some(MoveFault::NoRename)] {
        let e = run(&MoveRequest {
            fault,
            ..request(&from, &to)
        })
        .unwrap_err();
        assert!(
            matches!(
                e,
                StorageError::InvalidPath {
                    reason: PathRejection::Link,
                    ..
                }
            ),
            "{e}"
        );
        assert!(!to.exists());
    }
}

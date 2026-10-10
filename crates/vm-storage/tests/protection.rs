//! Disk protection during a run (ADR-0025, backlog F1-09): baseline opened
//! with `FILE_SHARE_READ` only, writer detector, hash verification before
//! start, overlay without `FILE_SHARE_DELETE`.
//!
//! The tests use synthetic files (no QEMU): "another process" is simulated
//! by a handle opened in the same process, which is equivalent for Windows
//! share modes (sharing is a property of the open).

mod common;

use std::path::PathBuf;

use vm_storage::{BaselineId, Catalog, RunId, StorageError, VmId, WriterProbe};

struct Fx {
    _dir: tempfile::TempDir,
    cat: Catalog,
    vm: VmId,
    baseline: BaselineId,
    run: RunId,
    base: PathBuf,
    base_vars: PathBuf,
    overlay: PathBuf,
}

fn fixture() -> Fx {
    let dir = common::tempdir();
    let mut cat = Catalog::open(dir.path()).unwrap();
    let vm = common::new_vm(&mut cat, "vm");
    let baseline = common::baseline(&mut cat, &vm, false);
    let run = common::run_from(&mut cat, &vm, &baseline);
    let (base, base_vars, overlay) = cat
        .read(|tx| {
            let b = tx.baseline(&baseline)?;
            let d = tx.disk(&b.base_disk_id)?.managed_path;
            let v = tx.firmware(&b.vars_fw_id.unwrap())?.managed_path;
            let o = tx.disk(&run.overlay_disk_id.unwrap())?.managed_path;
            Ok((d, v, o))
        })
        .unwrap();
    let r = |m| cat.layout().resolve(m).unwrap();
    let (base, base_vars, overlay) = (r(&base), r(&base_vars), r(&overlay));
    Fx {
        _dir: dir,
        cat,
        vm,
        baseline,
        run: run.run_id,
        base,
        base_vars,
        overlay,
    }
}

#[allow(clippy::permissions_set_readonly_false)]
fn clear_readonly(p: &std::path::Path) {
    let mut perm = std::fs::metadata(p).unwrap().permissions();
    perm.set_readonly(false);
    std::fs::set_permissions(p, perm).unwrap();
}

#[test]
fn guard_verifies_baseline_hash_through_the_handle() {
    let mut fx = fixture();
    let g = fx.cat.guard_run(&fx.run).unwrap();
    let b = g.baseline().expect("baseline guard");
    assert_eq!(b.baseline_id(), fx.baseline);
    assert_eq!(b.report().hash_verified.len(), 2, "base disk and vars");
    assert!(b.report().readonly_restored.is_empty());
    assert_eq!(g.no_delete_paths().count(), 2, "the run's overlay and vars");
    drop(g);
    // A second guard after release succeeds (no leftover handle).
    fx.cat.guard_run(&fx.run).unwrap();
}

#[test]
fn altered_baseline_is_refused_before_start() {
    let mut fx = fixture();
    clear_readonly(&fx.base);
    std::fs::write(&fx.base, b"QFI\xfb base altered outside the catalog").unwrap();
    let err = fx.cat.guard_run(&fx.run).unwrap_err();
    assert!(
        matches!(&err, StorageError::IntegrityMismatch { path, .. } if path.ends_with("disk.qcow2")),
        "{err:?}"
    );
    assert_eq!(err.public_code(), Some("state_conflict"));
    // The baseline's vars are verified too.
    let mut fx = fixture();
    clear_readonly(&fx.base_vars);
    std::fs::write(&fx.base_vars, b"altered OVMF_VARS").unwrap();
    let err = fx.cat.guard_baseline(&fx.baseline).unwrap_err();
    assert!(
        matches!(&err, StorageError::IntegrityMismatch { path, .. } if path.ends_with("vars.fd")),
        "{err:?}"
    );
}

#[test]
fn missing_readonly_attribute_is_restored_and_reported() {
    let mut fx = fixture();
    clear_readonly(&fx.base);
    let g = fx.cat.guard_baseline(&fx.baseline).unwrap();
    assert_eq!(g.report().readonly_restored.len(), 1);
    assert!(
        std::fs::metadata(&fx.base)
            .unwrap()
            .permissions()
            .readonly()
    );
}

#[test]
fn ended_run_cannot_be_guarded() {
    let mut fx = fixture();
    fx.cat
        .with_tx(|tx| {
            tx.end_run(
                &fx.run,
                vm_storage::RunState::Stopped,
                vm_storage::TerminationCause::Shutdown,
            )
        })
        .unwrap();
    let err = fx.cat.guard_run(&fx.run).unwrap_err();
    assert!(matches!(err, StorageError::StateConflict(_)), "{err:?}");
}

#[cfg(windows)]
mod windows {
    use std::fs::OpenOptions;
    use std::os::windows::fs::OpenOptionsExt;

    use super::*;

    const SHARE_ALL: u32 = 0x1 | 0x2 | 0x4;
    const SHARING_VIOLATION: i32 = 32;

    fn open_rw_share_all(p: &std::path::Path) -> std::io::Result<std::fs::File> {
        OpenOptions::new()
            .read(true)
            .write(true)
            .share_mode(SHARE_ALL)
            .open(p)
    }

    #[test]
    fn guarded_baseline_rejects_writers_rename_and_delete() {
        let mut fx = fixture();
        let before = vm_storage::sha256_file(&fx.base).unwrap();
        let _g = fx.cat.guard_run(&fx.run).unwrap();
        // Writing (even after first clearing the read-only attribute).
        clear_readonly(&fx.base);
        let e = open_rw_share_all(&fx.base).unwrap_err();
        assert_eq!(e.raw_os_error(), Some(SHARING_VIOLATION), "{e:?}");
        let e = open_rw_share_all(&fx.base_vars).unwrap_err();
        assert!(e.raw_os_error().is_some(), "{e:?}");
        // Rename and delete.
        let e = std::fs::rename(&fx.base, fx.base.with_extension("moved")).unwrap_err();
        assert_eq!(e.raw_os_error(), Some(SHARING_VIOLATION), "{e:?}");
        let e = std::fs::remove_file(&fx.base).unwrap_err();
        assert_eq!(e.raw_os_error(), Some(SHARING_VIOLATION), "{e:?}");
        // Shared reading (as QEMU does with the base) remains possible.
        assert_eq!(vm_storage::sha256_file(&fx.base).unwrap(), before);
    }

    #[test]
    fn writer_detector_refuses_start_and_preparation() {
        let mut fx = fixture();
        assert_eq!(
            fx.cat.probe_baseline_writers(&fx.baseline).unwrap(),
            WriterProbe::Clear
        );
        clear_readonly(&fx.base);
        let writer = open_rw_share_all(&fx.base).unwrap();
        match fx.cat.probe_baseline_writers(&fx.baseline).unwrap() {
            WriterProbe::Busy(paths) => {
                assert_eq!(paths.len(), 1);
                assert!(paths[0].as_str().ends_with("disk.qcow2"));
            }
            other => panic!("writer not detected: {other:?}"),
        }
        let err = fx.cat.guard_run(&fx.run).unwrap_err();
        assert!(matches!(err, StorageError::VmBusy(_)), "{err:?}");
        assert_eq!(err.public_code(), Some("vm_busy"));
        // Preparing a new run is refused too, without leaving open
        // intents.
        let err = fx
            .cat
            .prepare_run_from_baseline(&fx.vm, &fx.baseline, |t, _| {
                common::write_bytes(t, b"overlay")
            })
            .unwrap_err();
        assert!(matches!(err, StorageError::VmBusy(_)), "{err:?}");
        assert!(fx.cat.open_intents().unwrap().is_empty());
        drop(writer);
        assert_eq!(
            fx.cat.probe_baseline_writers(&fx.baseline).unwrap(),
            WriterProbe::Clear
        );
    }

    #[test]
    fn overlay_in_use_cannot_be_renamed_or_deleted_but_remains_writable() {
        let mut fx = fixture();
        let _g = fx.cat.guard_run(&fx.run).unwrap();
        let e = std::fs::rename(&fx.overlay, fx.overlay.with_extension("moved")).unwrap_err();
        assert_eq!(e.raw_os_error(), Some(SHARING_VIOLATION), "{e:?}");
        let e = std::fs::remove_file(&fx.overlay).unwrap_err();
        assert_eq!(e.raw_os_error(), Some(SHARING_VIOLATION), "{e:?}");
        // Declared limitation (ADR-0025 point 4, open decision): QEMU must
        // write the overlay, so another process of the same user can do so
        // too while the guard is active.
        let mut other = OpenOptions::new()
            .write(true)
            .share_mode(0x1 | 0x2)
            .open(&fx.overlay)
            .expect("third-party write to the in-use overlay: declared limitation");
        std::io::Write::write_all(&mut other, b"third-party write").unwrap();
    }

    #[test]
    fn guarded_baseline_cannot_be_deleted_through_the_journal() {
        let mut fx = fixture();
        let _g = fx.cat.guard_baseline(&fx.baseline).unwrap();
        // Attempt to move the base into a deletion's temporary: the rename
        // fails and the file stays in place.
        let rel = fx
            .cat
            .layout()
            .managed_from_absolute(&fx.base)
            .expect("managed path of the base");
        let i = fx
            .cat
            .begin_intent(vm_storage::IntentSpec {
                kind: vm_storage::IntentKind::DeleteFile,
                vm_id: Some(fx.vm),
                operation_id: None,
                target_kind: vm_storage::TargetKind::None,
                target_id: None,
                final_path: Some(rel),
                read_only: false,
            })
            .unwrap();
        let err = fx.cat.mark_applied(&i).unwrap_err();
        assert!(matches!(err, StorageError::Io { .. }), "{err:?}");
        fx.cat.abort(&i).unwrap();
        assert!(fx.base.is_file());
    }
}

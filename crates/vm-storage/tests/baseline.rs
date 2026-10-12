//! F3: baselines with the VM off, current state, restore and deletion
//! through the three-step API (begin → job → finish), with the file work
//! simulated by closures (`qemu-img` in the product). Includes the crash
//! points of every operation (fault injection, catalog dropped without
//! rollback, reopened) and the negative cases (writer detector, tampering,
//! referenced or protected baseline, VM not stopped).

mod common;

use std::path::{Path, PathBuf};

use vm_storage::{
    BaselineId, Catalog, CatalogLayout, CreateBaseline, CurrentStateFiles, FaultAction, FaultPoint,
    Lifecycle, ManagedPath, RestoreBaseline, RunRecord, RunState, StorageError, VmId,
    qcow2_backing,
};

/// Minimal QCOW2 v3 header (4 KiB) with an optional backing reference and
/// format extension, followed by `payload` (the "guest data").
fn qcow2(backing: Option<&str>, payload: &[u8]) -> Vec<u8> {
    let mut h = vec![0u8; 4096];
    h[0..4].copy_from_slice(&[0x51, 0x46, 0x49, 0xfb]);
    h[4..8].copy_from_slice(&3u32.to_be_bytes());
    h[100..104].copy_from_slice(&104u32.to_be_bytes());
    if let Some(name) = backing {
        let fmt = b"qcow2";
        h[104..108].copy_from_slice(&0xe279_2acau32.to_be_bytes());
        h[108..112].copy_from_slice(&(fmt.len() as u32).to_be_bytes());
        h[112..112 + fmt.len()].copy_from_slice(fmt);
        let off = 112 + 8 + 8;
        h[8..16].copy_from_slice(&(off as u64).to_be_bytes());
        h[16..20].copy_from_slice(&(name.len() as u32).to_be_bytes());
        h[off..off + name.len()].copy_from_slice(name.as_bytes());
    }
    h.extend_from_slice(payload);
    h
}

fn io(e: std::io::Error, p: &Path) -> StorageError {
    StorageError::Io {
        path: p.to_path_buf(),
        source: e,
    }
}

/// "convert": the standalone image is the guest-visible content. For a
/// fake overlay the content is its own payload if not empty, otherwise the
/// payload of its base (resolved like QEMU, relative to the overlay).
fn convert(src: &Path, dst: &Path) -> vm_storage::Result<u64> {
    let bytes = std::fs::read(src).map_err(|e| io(e, src))?;
    let payload = bytes[4096..].to_vec();
    let content = match qcow2_backing(src)? {
        Some((r, _)) if payload.is_empty() => {
            let base = r
                .split('/')
                .fold(src.parent().unwrap().to_path_buf(), |p, c| p.join(c));
            let b = std::fs::read(&base).map_err(|e| io(e, &base))?;
            b[4096..].to_vec()
        }
        _ => payload,
    };
    std::fs::write(dst, qcow2(None, &content)).map_err(|e| io(e, dst))?;
    Ok(20 << 30)
}

fn create_overlay(backing: &str, _size: u64, dst: &Path) -> vm_storage::Result<()> {
    std::fs::write(dst, qcow2(Some(backing), b"")).map_err(|e| io(e, dst))
}

struct Fx {
    dir: tempfile::TempDir,
    vm: VmId,
}

impl Fx {
    fn new(cat: &mut Catalog, dir: tempfile::TempDir) -> Self {
        let vm = common::new_vm(cat, "vm");
        let fx = Self { dir, vm };
        std::fs::create_dir_all(fx.path("firmware")).unwrap();
        std::fs::write(fx.path("primary.qcow2"), qcow2(None, b"installed system")).unwrap();
        std::fs::write(fx.path("firmware/primary.vars.fd"), b"VARS v1").unwrap();
        fx
    }

    fn path(&self, rel: &str) -> PathBuf {
        rel.split('/').fold(
            self.dir.path().join("vms").join(self.vm.to_string()),
            |p, c| p.join(c),
        )
    }

    fn standalone(&self) -> CurrentStateFiles {
        CurrentStateFiles {
            disk: CatalogLayout::primary_disk(&self.vm),
            vars: Some(CatalogLayout::primary_vars(&self.vm)),
        }
    }

    fn create(&self, cat: &mut Catalog, label: &str) -> vm_storage::Result<BaselineId> {
        let job = cat.begin_create_baseline(CreateBaseline {
            vm_id: self.vm,
            label: label.into(),
            clean_shutdown: true,
            protected: false,
            standalone: self.standalone(),
        })?;
        let done = job.run(convert);
        cat.finish_create_baseline(done).map(|b| b.baseline_id)
    }

    fn restore(&self, cat: &mut Catalog, b: &BaselineId) -> vm_storage::Result<()> {
        let job = cat.begin_restore(RestoreBaseline {
            vm_id: self.vm,
            baseline_id: *b,
            standalone: self.standalone(),
        })?;
        let done = job.run(create_overlay);
        cat.finish_restore(done).map(|_| ())
    }

    /// Guest-visible content of the current state.
    fn current_content(&self, cat: &mut Catalog) -> Vec<u8> {
        let rel = cat
            .read(|tx| {
                Ok(match tx.current_state(&self.vm)? {
                    Some(cs) => Some(tx.disk(&cs.overlay_disk_id)?.managed_path),
                    None => None,
                })
            })
            .unwrap();
        let p = match rel {
            Some(r) => cat.layout().resolve(&r).unwrap(),
            None => self.path("primary.qcow2"),
        };
        let tmp = self.dir.path().join("content.qcow2");
        let _ = std::fs::remove_file(&tmp);
        convert(&p, &tmp).unwrap();
        std::fs::read(&tmp).unwrap()[4096..].to_vec()
    }

    /// Guest write into the current overlay (as QEMU would).
    fn guest_write(&self, cat: &mut Catalog, data: &[u8]) {
        let rel = cat
            .read(|tx| {
                let cs = tx.current_state(&self.vm)?.unwrap();
                Ok(tx.disk(&cs.overlay_disk_id)?.managed_path)
            })
            .unwrap();
        let p = cat.layout().resolve(&rel).unwrap();
        let backing = qcow2_backing(&p).unwrap().map(|(r, _)| r);
        std::fs::write(&p, qcow2(backing.as_deref(), data)).unwrap();
    }

    fn files_under(&self, rel: &str) -> Vec<String> {
        let mut out = Vec::new();
        walk(&self.path(rel), &mut out);
        out.sort();
        out
    }
}

fn walk(p: &Path, out: &mut Vec<String>) {
    if let Ok(rd) = std::fs::read_dir(p) {
        for e in rd.flatten() {
            let path = e.path();
            if path.is_dir() {
                walk(&path, out);
            } else {
                out.push(path.display().to_string());
            }
        }
    }
}

fn tmp_count(dir: &Path) -> usize {
    std::fs::read_dir(dir.join("tmp")).unwrap().count()
}

fn base_hash(cat: &mut Catalog, b: &BaselineId) -> String {
    common::base_hash(cat, b)
}

#[test]
fn create_restore_and_create_from_an_overlay() {
    let dir = common::tempdir();
    let mut cat = Catalog::open(dir.path()).unwrap();
    let fx = Fx::new(&mut cat, dir);
    let b1 = fx.create(&mut cat, "clean install").unwrap();
    let h1 = base_hash(&mut cat, &b1);
    // Standalone image, read-only, hashed, manifest consistent.
    let (disk_rel, vars_rel) = cat
        .read(|tx| {
            let b = tx.baseline(&b1)?;
            Ok((
                tx.disk(&b.base_disk_id)?.managed_path,
                tx.firmware(&b.vars_fw_id.unwrap())?.managed_path,
            ))
        })
        .unwrap();
    let disk = cat.layout().resolve(&disk_rel).unwrap();
    assert_eq!(qcow2_backing(&disk).unwrap(), None, "standalone image");
    assert!(std::fs::metadata(&disk).unwrap().permissions().readonly());
    assert!(
        std::fs::metadata(cat.layout().resolve(&vars_rel).unwrap())
            .unwrap()
            .permissions()
            .readonly()
    );
    assert!(cat.verify_baseline_manifest(&b1, true).unwrap().is_empty());
    assert!(cat.last_exit_clean(&fx.vm).unwrap(), "never ran: clean");

    // Restore: standalone disk discarded, overlay with a fixed backing reference.
    fx.restore(&mut cat, &b1).unwrap();
    assert!(!fx.path("primary.qcow2").exists());
    assert!(!fx.path("firmware/primary.vars.fd").exists());
    let ov = cat.baseline_overview(&fx.vm).unwrap();
    assert_eq!(ov.current, Some(b1));
    assert_eq!(ov.count, 1);
    assert_eq!(fx.current_content(&mut cat), b"installed system");
    let infos = cat.baseline_infos(&fx.vm).unwrap();
    assert!(infos[0].current && infos[0].in_use);

    // Guest changes, second baseline from the overlay: standalone, derived.
    fx.guest_write(&mut cat, b"with the app installed");
    let b2 = fx.create(&mut cat, "app installed").unwrap();
    let infos = cat.baseline_infos(&fx.vm).unwrap();
    let i2 = infos.iter().find(|i| i.record.baseline_id == b2).unwrap();
    assert_eq!(i2.derived_from, Some(b1));
    assert!(!i2.current && !i2.in_use);
    let d2 = cat.layout().resolve(&i2.disk.managed_path).unwrap();
    assert_eq!(qcow2_backing(&d2).unwrap(), None, "no chains");

    // Restore b2, then b1: content follows, base hashes never change.
    fx.restore(&mut cat, &b2).unwrap();
    assert_eq!(fx.current_content(&mut cat), b"with the app installed");
    fx.guest_write(&mut cat, b"scratch");
    fx.restore(&mut cat, &b1).unwrap();
    assert_eq!(fx.current_content(&mut cat), b"installed system");
    assert_eq!(base_hash(&mut cat, &b1), h1);
    // One overlay and one vars file only (old ones discarded).
    assert_eq!(fx.files_under("overlays").len(), 1);
    assert_eq!(fx.files_under("firmware").len(), 1);
    assert_eq!(tmp_count(fx.dir.path()), 0);
    assert!(cat.open_intents().unwrap().is_empty());
    assert!(cat.verify_vm_manifest(&fx.vm).unwrap().is_empty());
}

#[test]
fn deletion_refuses_referenced_and_protected_baselines() {
    let dir = common::tempdir();
    let mut cat = Catalog::open(dir.path()).unwrap();
    let fx = Fx::new(&mut cat, dir);
    let b1 = fx.create(&mut cat, "one").unwrap();
    let b2 = fx.create(&mut cat, "two").unwrap();
    fx.restore(&mut cat, &b1).unwrap();
    let e = cat.delete_baseline_files(&fx.vm, &b1).unwrap_err();
    assert!(
        matches!(e, StorageError::Referenced("baseline_referenced")),
        "{e:?}"
    );
    cat.delete_baseline_files(&fx.vm, &b2).unwrap();
    assert!(!fx.path(&format!("base/{b2}")).exists());
    // Protected baseline (created by the storage API with `protected`).
    let job = cat
        .begin_create_baseline(CreateBaseline {
            vm_id: fx.vm,
            label: "initial".into(),
            clean_shutdown: true,
            protected: true,
            standalone: fx.standalone(),
        })
        .unwrap();
    let p = cat.finish_create_baseline(job.run(convert)).unwrap();
    let e = cat
        .delete_baseline_files(&fx.vm, &p.baseline_id)
        .unwrap_err();
    assert!(
        matches!(e, StorageError::Referenced("baseline_protected")),
        "{e:?}"
    );
    // After restoring another baseline, b1 is free again.
    fx.restore(&mut cat, &p.baseline_id).unwrap();
    cat.delete_baseline_files(&fx.vm, &b1).unwrap();
    assert_eq!(tmp_count(fx.dir.path()), 0);
    assert!(cat.open_intents().unwrap().is_empty());
}

#[test]
fn ended_runs_do_not_keep_a_baseline_or_an_overlay_alive() {
    let dir = common::tempdir();
    let mut cat = Catalog::open(dir.path()).unwrap();
    let fx = Fx::new(&mut cat, dir);
    let b1 = fx.create(&mut cat, "one").unwrap();
    let b2 = fx.create(&mut cat, "two").unwrap();
    fx.restore(&mut cat, &b1).unwrap();
    // A run on the overlay, ended.
    let (b, d, f) = cat.current_state_run_refs(&fx.vm).unwrap();
    let run = vm_storage::RunId::new_v7();
    cat.with_tx(|tx| {
        tx.insert_run(&RunRecord {
            run_id: run,
            vm_id: fx.vm,
            baseline_id: b,
            overlay_disk_id: d,
            vars_fw_id: f,
            state: RunState::Starting,
            started_at: vm_storage::now_rfc3339(),
            ended_at: None,
            termination_cause: None,
        })
    })
    .unwrap();
    // Open run: no restore, no deletion of the current baseline's overlay.
    let e = fx.restore(&mut cat, &b2).unwrap_err();
    assert!(matches!(e, StorageError::StateConflict(_)), "{e:?}");
    cat.with_tx(|tx| {
        tx.end_run(
            &run,
            RunState::Stopped,
            vm_storage::TerminationCause::Shutdown,
        )?;
        tx.set_run_clean_exit(&run, false)
    })
    .unwrap();
    assert!(
        !cat.last_exit_clean(&fx.vm).unwrap(),
        "forced end: not clean"
    );
    // Restore to b2: the old overlay (referenced by the ended run) goes.
    fx.restore(&mut cat, &b2).unwrap();
    cat.delete_baseline_files(&fx.vm, &b1).unwrap();
    let r = cat.read(|tx| tx.run(&run)).unwrap();
    assert_eq!(
        (r.baseline_id, r.overlay_disk_id, r.vars_fw_id),
        (None, None, None)
    );
}

#[test]
fn baseline_and_restore_require_a_stopped_vm() {
    let dir = common::tempdir();
    let mut cat = Catalog::open(dir.path()).unwrap();
    let fx = Fx::new(&mut cat, dir);
    let b1 = fx.create(&mut cat, "one").unwrap();
    cat.with_tx(|tx| tx.set_vm_lifecycle(&fx.vm, Lifecycle::Running))
        .unwrap();
    for e in [
        fx.create(&mut cat, "two").unwrap_err(),
        fx.restore(&mut cat, &b1).unwrap_err(),
        cat.delete_baseline_files(&fx.vm, &b1).unwrap_err(),
    ] {
        assert!(matches!(e, StorageError::StateConflict(_)), "{e:?}");
    }
    assert!(cat.open_intents().unwrap().is_empty());
}

#[test]
fn tampered_baseline_is_detected_at_restore_and_at_start() {
    let dir = common::tempdir();
    let mut cat = Catalog::open(dir.path()).unwrap();
    let fx = Fx::new(&mut cat, dir);
    let b1 = fx.create(&mut cat, "one").unwrap();
    fx.restore(&mut cat, &b1).unwrap();
    let rel = cat
        .read(|tx| Ok(tx.disk(&tx.baseline(&b1)?.base_disk_id)?.managed_path))
        .unwrap();
    let p = cat.layout().resolve(&rel).unwrap();
    let mut perm = std::fs::metadata(&p).unwrap().permissions();
    #[allow(clippy::permissions_set_readonly_false)]
    perm.set_readonly(false);
    std::fs::set_permissions(&p, perm).unwrap();
    let mut bytes = std::fs::read(&p).unwrap();
    bytes.extend_from_slice(b" tampered");
    std::fs::write(&p, bytes).unwrap();
    // Start: refused before any process.
    let (b, d, f) = cat.current_state_run_refs(&fx.vm).unwrap();
    let run = vm_storage::RunId::new_v7();
    cat.with_tx(|tx| {
        tx.insert_run(&RunRecord {
            run_id: run,
            vm_id: fx.vm,
            baseline_id: b,
            overlay_disk_id: d,
            vars_fw_id: f,
            state: RunState::Starting,
            started_at: vm_storage::now_rfc3339(),
            ended_at: None,
            termination_cause: None,
        })
    })
    .unwrap();
    let e = cat.guard_current_state(&fx.vm, &run).unwrap_err();
    assert!(matches!(e, StorageError::IntegrityMismatch { .. }), "{e:?}");
    // Same check split as the service does it: guard opened without the
    // hash (fast), hash through the guard handles afterwards (worker).
    let mut files = cat
        .open_current_state(&fx.vm, &run)
        .unwrap()
        .expect("overlay state");
    let total = files.guard.baseline_bytes();
    assert!(total > 0);
    let e = files.guard.verify_baseline(&|| true, &|_| {}).unwrap_err();
    assert!(
        matches!(&e, StorageError::StateConflict(m) if m == vm_storage::HASH_CANCELLED),
        "{e:?}"
    );
    let read = std::cell::Cell::new(0u64);
    let e = files
        .guard
        .verify_baseline(&|| false, &|n| read.set(read.get() + n))
        .unwrap_err();
    assert!(matches!(e, StorageError::IntegrityMismatch { .. }), "{e:?}");
    assert!(read.get() > 0 && read.get() <= total);
    drop(files);
    cat.with_tx(|tx| {
        tx.end_run(
            &run,
            RunState::Failed,
            vm_storage::TerminationCause::StartFailed,
        )
    })
    .unwrap();
    // Restore: refused on the worker, nothing changed.
    let before = fx.files_under("");
    let e = fx.restore(&mut cat, &b1).unwrap_err();
    assert!(matches!(e, StorageError::IntegrityMismatch { .. }), "{e:?}");
    assert_eq!(fx.files_under(""), before);
    assert_eq!(tmp_count(fx.dir.path()), 0);
    assert!(cat.open_intents().unwrap().is_empty());
}

#[cfg(windows)]
#[test]
fn writer_detector_refuses_baseline_and_restore() {
    use std::os::windows::fs::OpenOptionsExt;
    let dir = common::tempdir();
    let mut cat = Catalog::open(dir.path()).unwrap();
    let fx = Fx::new(&mut cat, dir);
    let b1 = fx.create(&mut cat, "one").unwrap();
    // Another program holds the working disk open for writing.
    let w = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .share_mode(0x7)
        .open(fx.path("primary.qcow2"))
        .unwrap();
    let e = fx.create(&mut cat, "two").unwrap_err();
    assert!(matches!(e, StorageError::VmBusy(_)), "{e:?}");
    drop(w);
    // The base held open for writing: restore refused.
    let rel = cat
        .read(|tx| Ok(tx.disk(&tx.baseline(&b1)?.base_disk_id)?.managed_path))
        .unwrap();
    let p = cat.layout().resolve(&rel).unwrap();
    let w = std::fs::OpenOptions::new()
        .read(true)
        .share_mode(0x7)
        .open(&p)
        .unwrap();
    // A reader with full sharing does not block; a writer does.
    drop(w);
    #[allow(clippy::permissions_set_readonly_false)]
    {
        let mut perm = std::fs::metadata(&p).unwrap().permissions();
        perm.set_readonly(false);
        std::fs::set_permissions(&p, perm).unwrap();
    }
    let w = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .share_mode(0x7)
        .open(&p)
        .unwrap();
    let e = fx.restore(&mut cat, &b1).unwrap_err();
    assert!(matches!(e, StorageError::VmBusy(_)), "{e:?}");
    drop(w);
    assert!(cat.open_intents().unwrap().is_empty());
    // While a baseline creation holds the source, the disk cannot be
    // modified by others.
    let job = cat
        .begin_create_baseline(CreateBaseline {
            vm_id: fx.vm,
            label: "three".into(),
            clean_shutdown: true,
            protected: false,
            standalone: fx.standalone(),
        })
        .unwrap();
    assert!(
        std::fs::OpenOptions::new()
            .write(true)
            .open(fx.path("primary.qcow2"))
            .is_err()
    );
    cat.abort_baseline_job(job.run(convert)).unwrap();
    assert_eq!(tmp_count(fx.dir.path()), 0);
}

/// Snapshot of what must survive a crash: baseline rows and hashes,
/// guest-visible content of the current state.
fn state(cat: &mut Catalog, fx: &Fx) -> (Vec<(BaselineId, String)>, Vec<u8>, Option<BaselineId>) {
    let ids: Vec<BaselineId> = cat
        .read(|tx| {
            Ok(tx
                .baselines_of_vm(&fx.vm)?
                .into_iter()
                .map(|b| b.baseline_id)
                .collect())
        })
        .unwrap();
    let mut b: Vec<(BaselineId, String)> =
        ids.into_iter().map(|i| (i, base_hash(cat, &i))).collect();
    b.sort();
    let cur = cat.baseline_overview(&fx.vm).unwrap().current;
    (b, fx.current_content(cat), cur)
}

/// After reopening: no open intent, `tmp/` empty, every file under the VM
/// directory registered (or the standalone working files), VM not busy.
fn assert_reconciled(cat: &mut Catalog, fx: &Fx) {
    assert!(cat.open_intents().unwrap().is_empty(), "open intents");
    assert_eq!(tmp_count(fx.dir.path()), 0, "tmp/ not empty");
    let mut registered: Vec<String> = cat
        .read(|tx| {
            let mut v = Vec::new();
            for d in tx.disks_of_vm(&fx.vm)? {
                v.push(d.managed_path.to_string());
            }
            for b in tx.baselines_of_vm(&fx.vm)? {
                if let Some(f) = b.vars_fw_id {
                    v.push(tx.firmware(&f)?.managed_path.to_string());
                }
                v.push(CatalogLayout::baseline_manifest(&fx.vm, &b.baseline_id).to_string());
            }
            if let Some(cs) = tx.current_state(&fx.vm)?
                && let Some(f) = cs.vars_fw_id
            {
                v.push(tx.firmware(&f)?.managed_path.to_string());
            }
            Ok(v)
        })
        .unwrap();
    registered.push(CatalogLayout::vm_manifest(&fx.vm).to_string());
    registered.push(CatalogLayout::primary_disk(&fx.vm).to_string());
    registered.push(CatalogLayout::primary_vars(&fx.vm).to_string());
    let root = fx.dir.path().display().to_string();
    for f in fx.files_under("") {
        let rel = f[root.len() + 1..].replace('\\', "/");
        assert!(registered.contains(&rel), "orphan file {rel}");
    }
}

/// Runs `op` with a crash at `point`, drops the catalog, reopens it and
/// returns the reconciled catalog.
fn crash_and_reopen(
    dir: &Path,
    mut cat: Catalog,
    point: FaultPoint,
    op: impl FnOnce(&mut Catalog) -> vm_storage::Result<()>,
) -> Catalog {
    cat.inject_fault(point, FaultAction::Crash);
    let e = op(&mut cat).unwrap_err();
    assert!(
        matches!(e, StorageError::InjectedCrash(p) if p == point),
        "{e:?}"
    );
    drop(cat);
    Catalog::open(dir).unwrap()
}

#[test]
fn crash_during_baseline_creation_leaves_no_baseline_and_no_orphans() {
    for point in [
        FaultPoint::CreateBeforePublish,
        FaultPoint::CreateAfterDiskPublish,
        FaultPoint::CreateBeforeCommit,
        FaultPoint::AfterCommit,
    ] {
        let dir = common::tempdir();
        let mut cat = Catalog::open(dir.path()).unwrap();
        let fx = Fx::new(&mut cat, dir);
        let b1 = fx.create(&mut cat, "one").unwrap();
        fx.restore(&mut cat, &b1).unwrap();
        fx.guest_write(&mut cat, b"work in progress");
        let before = state(&mut cat, &fx);
        let mut cat = crash_and_reopen(fx.dir.path(), cat, point, |c| {
            fx.create(c, "two").map(|_| ())
        });
        let after = state(&mut cat, &fx);
        if point == FaultPoint::AfterCommit {
            // Committed: the baseline exists and is complete.
            assert_eq!(after.0.len(), 2, "{point:?}");
        } else {
            assert_eq!(after, before, "{point:?}");
        }
        assert_eq!(after.1, before.1, "current state untouched ({point:?})");
        assert_reconciled(&mut cat, &fx);
        // The VM is not left busy: a new creation succeeds.
        fx.create(&mut cat, "after the crash").unwrap();
    }
}

#[test]
fn crash_during_restore_keeps_the_old_or_the_new_state_never_none() {
    for point in [
        FaultPoint::RestoreBeforePublish,
        FaultPoint::RestoreAfterPublish,
        FaultPoint::RestoreAfterFirstDiscard,
        FaultPoint::RestoreBeforeCommit,
        FaultPoint::AfterCommit,
    ] {
        for from_overlay in [false, true] {
            let dir = common::tempdir();
            let mut cat = Catalog::open(dir.path()).unwrap();
            let fx = Fx::new(&mut cat, dir);
            let b1 = fx.create(&mut cat, "one").unwrap();
            if from_overlay {
                fx.restore(&mut cat, &b1).unwrap();
            }
            // Changes since the baseline (in the overlay or the standalone disk).
            if from_overlay {
                fx.guest_write(&mut cat, b"changed since");
            } else {
                std::fs::write(fx.path("primary.qcow2"), qcow2(None, b"changed since")).unwrap();
            }
            let before = state(&mut cat, &fx);
            let mut cat = crash_and_reopen(fx.dir.path(), cat, point, |c| fx.restore(c, &b1));
            let after = state(&mut cat, &fx);
            assert_eq!(after.0, before.0, "baselines untouched ({point:?})");
            if point == FaultPoint::AfterCommit {
                assert_eq!(after.1, b"installed system", "{point:?}");
                assert_eq!(after.2, Some(b1));
            } else {
                assert_eq!(after.1, b"changed since", "old state back ({point:?})");
                assert_eq!(after.2, before.2);
            }
            assert_reconciled(&mut cat, &fx);
            fx.restore(&mut cat, &b1).unwrap();
            assert_eq!(fx.current_content(&mut cat), b"installed system");
        }
    }
}

#[test]
fn crash_during_baseline_deletion_completes_or_keeps_it() {
    for point in [
        FaultPoint::DeleteAfterFirstMove,
        FaultPoint::DeleteBeforeCommit,
        FaultPoint::AfterCommit,
    ] {
        let dir = common::tempdir();
        let mut cat = Catalog::open(dir.path()).unwrap();
        let fx = Fx::new(&mut cat, dir);
        let b1 = fx.create(&mut cat, "one").unwrap();
        let b2 = fx.create(&mut cat, "two").unwrap();
        fx.restore(&mut cat, &b1).unwrap();
        let h1 = base_hash(&mut cat, &b1);
        let mut cat = crash_and_reopen(fx.dir.path(), cat, point, |c| {
            c.delete_baseline_files(&fx.vm, &b2)
        });
        let ids: Vec<BaselineId> = cat
            .read(|tx| {
                Ok(tx
                    .baselines_of_vm(&fx.vm)?
                    .into_iter()
                    .map(|b| b.baseline_id)
                    .collect())
            })
            .unwrap();
        // Once a file was moved the deletion rolls forward.
        assert_eq!(ids, vec![b1], "{point:?}");
        assert!(!fx.path(&format!("base/{b2}/disk.qcow2")).exists());
        assert_eq!(base_hash(&mut cat, &b1), h1);
        assert_eq!(fx.current_content(&mut cat), b"installed system");
        assert_reconciled(&mut cat, &fx);
    }
}

#[test]
fn interrupted_jobs_are_rolled_back_at_the_next_open() {
    // The process dies while the worker produces the image (job dropped).
    let dir = common::tempdir();
    let mut cat = Catalog::open(dir.path()).unwrap();
    let fx = Fx::new(&mut cat, dir);
    let b1 = fx.create(&mut cat, "one").unwrap();
    let job = cat
        .begin_create_baseline(CreateBaseline {
            vm_id: fx.vm,
            label: "two".into(),
            clean_shutdown: true,
            protected: false,
            standalone: fx.standalone(),
        })
        .unwrap();
    // Partial image written by an interrupted convert.
    std::fs::write(job.temp_disk(), qcow2(None, b"partial")).unwrap();
    drop(job);
    let job = {
        drop(cat);
        let mut cat = Catalog::open(fx.dir.path()).unwrap();
        assert_reconciled(&mut cat, &fx);
        let r = cat.begin_restore(RestoreBaseline {
            vm_id: fx.vm,
            baseline_id: b1,
            standalone: fx.standalone(),
        });
        (cat, r.unwrap())
    };
    let (cat, job) = job;
    drop(job);
    drop(cat);
    let mut cat = Catalog::open(fx.dir.path()).unwrap();
    assert_reconciled(&mut cat, &fx);
    assert_eq!(fx.current_content(&mut cat), b"installed system");
    assert_eq!(cat.baseline_overview(&fx.vm).unwrap().count, 1);
}

#[test]
fn backing_reference_has_the_fixed_shape() {
    let b = BaselineId::new_v7();
    let r = CatalogLayout::overlay_backing_ref(&b);
    assert_eq!(r, format!("../base/{b}/disk.qcow2"));
    // From the overlays directory it names the base disk of the baseline.
    let vm = VmId::new_v7();
    let overlay = CatalogLayout::state_overlay(&vm, &vm_storage::DiskId::new_v7());
    let dir = overlay.as_str().rsplit_once('/').unwrap().0;
    let resolved = format!("{}/{}", dir.rsplit_once('/').unwrap().0, &r[3..]);
    assert_eq!(
        ManagedPath::parse(&resolved).unwrap(),
        CatalogLayout::baseline_disk(&vm, &b)
    );
}

/// ADR-0028 (required evidence 3): until the swtpm state is part of
/// baselines, baseline creation and restore are refused for a TPM guest
/// (Windows 11 ARM64) with `unsupported`, before any file is touched; the
/// per-VM TPM state directory is private (0700 on Unix).
#[test]
fn tpm_guest_has_no_baselines_yet() {
    let dir = common::tempdir();
    let mut cat = Catalog::open(dir.path()).unwrap();
    let vm = cat
        .with_tx(|tx| {
            tx.insert_vm(&vm_storage::NewVm {
                name: "win11".into(),
                guest_os: "windows11_arm64".into(),
                config_version: 1,
                config: serde_json::json!({}),
            })
        })
        .unwrap();
    let fx = Fx { dir, vm };
    std::fs::create_dir_all(fx.path("firmware")).unwrap();
    std::fs::write(fx.path("primary.qcow2"), qcow2(None, b"installed")).unwrap();
    std::fs::write(fx.path("firmware/primary.vars.fd"), b"VARS").unwrap();
    let e = fx.create(&mut cat, "b").unwrap_err();
    assert!(
        matches!(e, StorageError::Unsupported(m) if m == vm_storage::TPM_BASELINE_UNSUPPORTED),
        "{e}"
    );
    let e = fx.restore(&mut cat, &BaselineId::new_v7()).unwrap_err();
    assert!(matches!(e, StorageError::Unsupported(_)), "{e}");
    let tpm = cat
        .layout()
        .ensure_private_dir(&CatalogLayout::tpm_state_dir(&fx.vm))
        .unwrap();
    assert_eq!(tpm, fx.path("tpm"));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = std::fs::metadata(&tpm).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o700);
    }
}

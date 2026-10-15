//! User snapshots (ADR-0032, intents `take_snapshot`, `revert_snapshot`,
//! `delete_snapshot`): tree and current position, re-parenting on delete,
//! UEFI variables and configuration restored by a revert, crash at every
//! step settled from the disk's QCOW2 snapshot table, refusals (overlay,
//! no disk, busy VM), baseline restore refused while snapshots exist,
//! storage usage.
//!
//! The disk is a minimal QCOW2 image written by [`FakeDisk`], which plays
//! `qemu-img snapshot -c/-a/-d` on its snapshot table and active L1 table;
//! the real `qemu-img` is exercised by the service's real-QEMU tests.

mod common;

use std::path::{Path, PathBuf};

use vm_storage::{
    Catalog, FaultAction, FaultPoint, IntentState, Lifecycle, ManagedPath, RecoveryOutcome,
    RevertSnapshot, Settled, SnapshotId, SnapshotOp, StorageError, TakeSnapshot, VmId,
};

/// Minimal QCOW2 v3 image: active L1 table at 0x10000, snapshot table at
/// 0x20000, snapshots' L1 tables from 0x30000.
#[derive(Debug, Clone, Default)]
struct FakeDisk {
    active: Vec<u64>,
    snaps: Vec<(String, Vec<u64>)>,
    next_cluster: u64,
}

impl FakeDisk {
    fn new() -> Self {
        Self {
            active: vec![0x50000, 0, 0x70000, 0],
            snaps: Vec::new(),
            next_cluster: 0x100000,
        }
    }

    fn bytes(&self) -> Vec<u8> {
        let size: u64 = 1 << 30;
        let mut img = vec![0u8; 0x60000];
        img[0..4].copy_from_slice(&[0x51, 0x46, 0x49, 0xfb]);
        img[4..8].copy_from_slice(&3u32.to_be_bytes());
        img[20..24].copy_from_slice(&16u32.to_be_bytes());
        img[24..32].copy_from_slice(&size.to_be_bytes());
        img[36..40].copy_from_slice(&(self.active.len() as u32).to_be_bytes());
        img[40..48].copy_from_slice(&0x10000u64.to_be_bytes());
        img[60..64].copy_from_slice(&(self.snaps.len() as u32).to_be_bytes());
        img[64..72].copy_from_slice(&0x20000u64.to_be_bytes());
        img[100..104].copy_from_slice(&104u32.to_be_bytes());
        for (i, e) in self.active.iter().enumerate() {
            let o = 0x10000 + i * 8;
            img[o..o + 8].copy_from_slice(&e.to_be_bytes());
        }
        let mut pos = 0x20000usize;
        for (k, (name, l1)) in self.snaps.iter().enumerate() {
            let l1_off = 0x30000 + k * 0x400;
            for (i, e) in l1.iter().enumerate() {
                let o = l1_off + i * 8;
                img[o..o + 8].copy_from_slice(&e.to_be_bytes());
            }
            let id = format!("{}", k + 1);
            img[pos..pos + 8].copy_from_slice(&(l1_off as u64).to_be_bytes());
            img[pos + 8..pos + 12].copy_from_slice(&(l1.len() as u32).to_be_bytes());
            img[pos + 12..pos + 14].copy_from_slice(&(id.len() as u16).to_be_bytes());
            img[pos + 14..pos + 16].copy_from_slice(&(name.len() as u16).to_be_bytes());
            img[pos + 36..pos + 40].copy_from_slice(&16u32.to_be_bytes());
            img[pos + 48..pos + 56].copy_from_slice(&size.to_be_bytes());
            let id_start = pos + 56;
            img[id_start..id_start + id.len()].copy_from_slice(id.as_bytes());
            let ns = id_start + id.len();
            img[ns..ns + name.len()].copy_from_slice(name.as_bytes());
            pos = (ns + name.len()).div_ceil(8) * 8;
        }
        img
    }

    fn load(path: &Path) -> Self {
        let info = vm_storage::qcow2_read_info(path).unwrap();
        let raw = std::fs::read(path).unwrap();
        let l1 = |off: u64, n: u32| -> Vec<u64> {
            (0..n as usize)
                .map(|i| {
                    let o = off as usize + i * 8;
                    u64::from_be_bytes(raw[o..o + 8].try_into().unwrap())
                })
                .collect()
        };
        Self {
            active: l1(info.l1_offset, info.l1_size),
            snaps: info
                .snapshots
                .iter()
                .map(|s| (s.name.clone(), l1(s.l1_offset, s.l1_size)))
                .collect(),
            next_cluster: 0x100000 + 0x10000 * u64::try_from(raw.len()).unwrap(),
        }
    }

    fn save(&self, path: &Path) {
        std::fs::write(path, self.bytes()).unwrap();
    }

    /// `qemu-img snapshot -c`.
    fn create(path: &Path, tag: &str) {
        let mut d = Self::load(path);
        let active = d.active.clone();
        d.snaps.push((tag.to_owned(), active));
        d.save(path);
    }

    /// `qemu-img snapshot -a`.
    fn apply(path: &Path, tag: &str) {
        let mut d = Self::load(path);
        let l1 = d.snaps.iter().find(|(n, _)| n == tag).unwrap().1.clone();
        d.active = l1;
        d.save(path);
    }

    /// `qemu-img snapshot -d`.
    fn delete(path: &Path, tag: &str) {
        let mut d = Self::load(path);
        d.snaps.retain(|(n, _)| n != tag);
        d.save(path);
    }

    /// The guest writes: a copy on write of the first L2 table.
    fn guest_write(path: &Path) {
        let mut d = Self::load(path);
        d.next_cluster += 0x10000;
        d.active[0] = d.next_cluster | (1 << 63);
        d.save(path);
    }

    fn tags(path: &Path) -> Vec<String> {
        Self::load(path).snaps.into_iter().map(|(n, _)| n).collect()
    }
}

struct Vm {
    id: VmId,
    root: PathBuf,
}

impl Vm {
    fn disk_rel(&self) -> ManagedPath {
        vm_storage::CatalogLayout::primary_disk(&self.id)
    }
    fn vars_rel(&self) -> ManagedPath {
        vm_storage::CatalogLayout::primary_vars(&self.id)
    }
    fn disk(&self) -> PathBuf {
        self.root.join(self.disk_rel().as_str())
    }
    fn vars(&self) -> PathBuf {
        self.root.join(self.vars_rel().as_str())
    }
}

fn vm_with_disk(cat: &mut Catalog, root: &Path, name: &str) -> Vm {
    let id = common::new_vm(cat, name);
    let vm = Vm {
        id,
        root: root.to_path_buf(),
    };
    std::fs::create_dir_all(vm.disk().parent().unwrap()).unwrap();
    FakeDisk::new().save(&vm.disk());
    std::fs::create_dir_all(vm.vars().parent().unwrap()).unwrap();
    std::fs::write(vm.vars(), b"vars v1").unwrap();
    vm
}

fn take_req(vm: &Vm, cat: &mut Catalog, name: &str, running: bool) -> TakeSnapshot {
    let row = cat.read(|tx| tx.vm(&vm.id)).unwrap();
    TakeSnapshot {
        vm_id: vm.id,
        name: name.into(),
        description: format!("about {name}"),
        running,
        clean_shutdown: true,
        config: row.config,
        config_revision: row.revision,
        disk: vm.disk_rel(),
        vars: Some(vm.vars_rel()),
        thumbnail: running.then(|| b"\x89PNG fake".to_vec()),
    }
}

fn take(cat: &mut Catalog, vm: &Vm, name: &str) -> SnapshotId {
    let req = take_req(vm, cat, name, false);
    let op = cat.begin_take_snapshot(req).unwrap();
    FakeDisk::create(op.disk(), op.tag());
    assert_eq!(cat.settle_snapshot(&op).unwrap(), Settled::Done);
    op.snapshot_id()
}

fn revert_req(cat: &mut Catalog, vm: &Vm, id: SnapshotId) -> RevertSnapshot {
    let row = cat.read(|tx| tx.vm(&vm.id)).unwrap();
    let snap = cat.read(|tx| tx.snapshot(&id)).unwrap();
    RevertSnapshot {
        vm_id: vm.id,
        snapshot_id: id,
        disk: vm.disk_rel(),
        vars: Some(vm.vars_rel()),
        config: snap.config,
        config_version: 1,
        expected_revision: row.revision,
    }
}

fn begin_revert(cat: &mut Catalog, vm: &Vm, id: SnapshotId) -> vm_storage::Result<SnapshotOp> {
    let req = revert_req(cat, vm, id);
    cat.begin_revert_snapshot(req)
}

fn begin_take(
    cat: &mut Catalog,
    vm: &Vm,
    name: &str,
    running: bool,
) -> vm_storage::Result<SnapshotOp> {
    let req = take_req(vm, cat, name, running);
    cat.begin_take_snapshot(req)
}

fn revert(cat: &mut Catalog, vm: &Vm, id: SnapshotId) {
    let op = begin_revert(cat, vm, id).unwrap();
    FakeDisk::apply(op.disk(), op.tag());
    assert_eq!(cat.settle_snapshot(&op).unwrap(), Settled::Done);
}

fn delete(cat: &mut Catalog, vm: &Vm, id: SnapshotId) {
    let op = cat
        .begin_delete_snapshot(&vm.id, &id, &vm.disk_rel())
        .unwrap();
    FakeDisk::delete(op.disk(), op.tag());
    assert_eq!(cat.settle_snapshot(&op).unwrap(), Settled::Done);
}

fn set_config(cat: &mut Catalog, vm: &Vm, cfg: serde_json::Value) {
    let rev = cat.read(|tx| tx.vm(&vm.id)).unwrap().revision;
    cat.with_tx(|tx| tx.update_vm_config(&vm.id, rev, 1, &cfg))
        .unwrap();
}

fn tmp_empty(root: &Path) -> bool {
    std::fs::read_dir(root.join("tmp")).unwrap().count() == 0
}

fn parents(cat: &mut Catalog, vm: &Vm) -> Vec<(SnapshotId, Option<SnapshotId>)> {
    cat.snapshot_list(&vm.id)
        .unwrap()
        .snapshots
        .iter()
        .map(|s| (s.snapshot_id, s.parent_id))
        .collect()
}

#[test]
fn take_revert_delete_build_and_keep_the_tree() {
    let dir = common::tempdir();
    let mut cat = Catalog::open(dir.path()).unwrap();
    let vm = vm_with_disk(&mut cat, dir.path(), "tree");
    // a (root) -> b ; revert to a ; c (child of a) ; delete a: b and c become roots.
    let a = take(&mut cat, &vm, "a");
    let list = cat.snapshot_list(&vm.id).unwrap();
    assert_eq!(list.current, Some(a));
    let s = &list.snapshots[0];
    assert_eq!(s.name, "a");
    assert_eq!(s.description, "about a");
    assert!(!s.taken_while_running);
    assert!(s.clean_shutdown);
    assert_eq!(s.vars_bytes, 7);
    assert!(s.vars_sha256.is_some());
    let snap_vars = cat.snapshot_vars(&vm.id, &a).unwrap().unwrap();
    assert_eq!(std::fs::read(&snap_vars).unwrap(), b"vars v1");
    assert!(
        std::fs::metadata(&snap_vars)
            .unwrap()
            .permissions()
            .readonly()
    );

    FakeDisk::guest_write(&vm.disk());
    std::fs::write(vm.vars(), b"vars v2").unwrap();
    set_config(&mut cat, &vm, serde_json::json!({"cpus": 4}));
    let b = take(&mut cat, &vm, "b");
    assert_eq!(parents(&mut cat, &vm), vec![(a, None), (b, Some(a))]);
    assert_eq!(cat.snapshot_list(&vm.id).unwrap().current, Some(b));

    FakeDisk::guest_write(&vm.disk());
    std::fs::write(vm.vars(), b"vars v3").unwrap();
    revert(&mut cat, &vm, a);
    // Disk, variables and configuration of `a`; position at `a`.
    assert_eq!(
        vm_storage::qcow2_active_matches(&vm.disk(), &a.to_string()).unwrap(),
        Some(true)
    );
    assert_eq!(std::fs::read(vm.vars()).unwrap(), b"vars v1");
    assert!(
        !std::fs::metadata(vm.vars())
            .unwrap()
            .permissions()
            .readonly()
    );
    let row = cat.read(|tx| tx.vm(&vm.id)).unwrap();
    assert_eq!(row.config["cpus"], 2);
    assert_eq!(row.revision, 3);
    assert_eq!(cat.snapshot_list(&vm.id).unwrap().current, Some(a));

    let c = take(&mut cat, &vm, "c");
    assert_eq!(
        parents(&mut cat, &vm),
        vec![(a, None), (b, Some(a)), (c, Some(a))]
    );
    delete(&mut cat, &vm, a);
    assert_eq!(parents(&mut cat, &vm), vec![(b, None), (c, None)]);
    assert_eq!(cat.snapshot_list(&vm.id).unwrap().current, Some(c));
    assert!(
        !dir.path()
            .join(vm_storage::CatalogLayout::snapshot_dir(&vm.id, &a).as_str())
            .exists()
    );
    assert_eq!(
        FakeDisk::tags(&vm.disk()),
        vec![b.to_string(), c.to_string()]
    );
    // Deleting the head moves it to the parent (none here).
    delete(&mut cat, &vm, c);
    assert_eq!(cat.snapshot_list(&vm.id).unwrap().current, None);
    // b still reverts with its own state.
    revert(&mut cat, &vm, b);
    assert_eq!(std::fs::read(vm.vars()).unwrap(), b"vars v2");
    assert_eq!(cat.read(|tx| tx.vm(&vm.id)).unwrap().config["cpus"], 4);
    assert!(cat.open_intents().unwrap().is_empty());
    assert!(tmp_empty(dir.path()));
    // Usage counts the snapshot files.
    let usage = cat.storage_usage(Some(&vm.id)).unwrap();
    assert_eq!(usage.snapshots, 7);
}

#[test]
fn rename_and_running_snapshot_with_thumbnail() {
    let dir = common::tempdir();
    let mut cat = Catalog::open(dir.path()).unwrap();
    let vm = vm_with_disk(&mut cat, dir.path(), "live");
    let req = take_req(&vm, &mut cat, "live", true);
    let op = cat.begin_take_snapshot(req).unwrap();
    FakeDisk::create(op.disk(), op.tag());
    cat.settle_snapshot(&op).unwrap();
    let id = op.snapshot_id();
    let s = cat.read(|tx| tx.snapshot(&id)).unwrap();
    assert!(s.taken_while_running);
    assert!(!s.clean_shutdown, "a running snapshot is never clean");
    assert_eq!(s.thumbnail_bytes, 9);
    let thumb = cat.snapshot_thumbnail(&vm.id, &id).unwrap().unwrap();
    assert_eq!(std::fs::read(thumb).unwrap(), b"\x89PNG fake");
    let s = cat
        .update_snapshot_text(&vm.id, &id, "renamed", "new text")
        .unwrap();
    assert_eq!(
        (s.name.as_str(), s.description.as_str()),
        ("renamed", "new text")
    );
    // Another VM's snapshot is not found through this VM.
    let other = common::new_vm(&mut cat, "other");
    assert!(matches!(
        cat.update_snapshot_text(&other, &id, "x", ""),
        Err(StorageError::NotFound { .. })
    ));
    // Oversized thumbnail refused.
    let mut req = take_req(&vm, &mut cat, "big", true);
    req.thumbnail = Some(vec![0u8; vm_storage::MAX_THUMBNAIL_BYTES + 1]);
    assert!(cat.begin_take_snapshot(req).is_err());
}

#[test]
fn failed_or_cancelled_disk_operations_change_nothing() {
    let dir = common::tempdir();
    let mut cat = Catalog::open(dir.path()).unwrap();
    let vm = vm_with_disk(&mut cat, dir.path(), "cancel");
    let a = take(&mut cat, &vm, "a");
    // Take: the tool failed, no tag.
    let req = take_req(&vm, &mut cat, "x", false);
    let op = cat.begin_take_snapshot(req).unwrap();
    // The VM is busy for other snapshot operations meanwhile.
    assert!(matches!(
        cat.begin_delete_snapshot(&vm.id, &a, &vm.disk_rel()),
        Err(StorageError::VmBusy(_))
    ));
    assert_eq!(cat.settle_snapshot(&op).unwrap(), Settled::NotDone);
    assert_eq!(cat.snapshot_list(&vm.id).unwrap().snapshots.len(), 1);
    // Revert: the disk was not reverted, the variables stay.
    FakeDisk::guest_write(&vm.disk());
    std::fs::write(vm.vars(), b"vars now").unwrap();
    let op = begin_revert(&mut cat, &vm, a).unwrap();
    assert_eq!(cat.settle_snapshot(&op).unwrap(), Settled::NotDone);
    assert_eq!(std::fs::read(vm.vars()).unwrap(), b"vars now");
    assert_eq!(cat.read(|tx| tx.vm(&vm.id)).unwrap().revision, 1);
    // Delete: the tag is still there.
    let op = cat
        .begin_delete_snapshot(&vm.id, &a, &vm.disk_rel())
        .unwrap();
    assert_eq!(cat.settle_snapshot(&op).unwrap(), Settled::NotDone);
    assert_eq!(cat.snapshot_list(&vm.id).unwrap().snapshots.len(), 1);
    assert!(cat.snapshot_vars(&vm.id, &a).unwrap().unwrap().exists());
    // A take whose tool reported a failure but created the tag (a QMP reply
    // lost when the process died) is registered.
    let req = take_req(&vm, &mut cat, "late", true);
    let op = cat.begin_take_snapshot(req).unwrap();
    FakeDisk::create(op.disk(), op.tag());
    assert_eq!(cat.settle_snapshot(&op).unwrap(), Settled::Done);
    assert!(cat.open_intents().unwrap().is_empty());
    assert!(tmp_empty(dir.path()));
}

#[test]
fn refusals_overlay_no_disk_and_baseline_restore() {
    let dir = common::tempdir();
    let mut cat = Catalog::open(dir.path()).unwrap();
    // No disk yet.
    let id = common::new_vm(&mut cat, "fresh");
    let fresh = Vm {
        id,
        root: dir.path().to_path_buf(),
    };
    let req = take_req(&fresh, &mut cat, "x", false);
    match cat.begin_take_snapshot(req) {
        Err(StorageError::StateConflict(m)) => assert_eq!(m, vm_storage::SNAPSHOT_NO_DISK),
        other => panic!("{other:?}"),
    }
    // Baseline restore refused while snapshots exist.
    let vm = vm_with_disk(&mut cat, dir.path(), "with-snapshots");
    let b = common::baseline(&mut cat, &vm.id, false);
    let _a = take(&mut cat, &vm, "a");
    let r = cat.begin_restore(vm_storage::RestoreBaseline {
        vm_id: vm.id,
        baseline_id: b,
        standalone: vm_storage::CurrentStateFiles {
            disk: vm.disk_rel(),
            vars: Some(vm.vars_rel()),
        },
    });
    match r {
        Err(StorageError::StateConflict(m)) => {
            assert_eq!(m, vm_storage::RESTORE_WITH_SNAPSHOTS);
        }
        other => panic!("{other:?}"),
    }
    // A VM on an overlay of a baseline: snapshots refused.
    let ov = vm_with_disk(&mut cat, dir.path(), "overlay");
    let b2 = common::baseline(&mut cat, &ov.id, false);
    let base_disk = cat.read(|tx| Ok(tx.baseline(&b2)?.base_disk_id)).unwrap();
    let overlay = vm_storage::DiskId::new_v7();
    cat.with_tx(|tx| {
        tx.insert_disk(&vm_storage::DiskRecord {
            disk_id: overlay,
            vm_id: ov.id,
            role: vm_storage::DiskRole::Overlay,
            backing_disk_id: Some(base_disk),
            virtual_size_bytes: 1 << 30,
            allocated_bytes: 0,
            managed_path: vm_storage::CatalogLayout::state_overlay(&ov.id, &overlay),
            sha256: None,
            status: vm_storage::DiskStatus::Ready,
            created_at: vm_storage::now_rfc3339(),
        })?;
        tx.set_current_state(&vm_storage::CurrentStateRecord {
            vm_id: ov.id,
            baseline_id: b2,
            overlay_disk_id: overlay,
            vars_fw_id: None,
            restored_at: vm_storage::now_rfc3339(),
        })
    })
    .unwrap();
    let req = take_req(&ov, &mut cat, "x", false);
    match cat.begin_take_snapshot(req) {
        Err(StorageError::StateConflict(m)) => {
            assert_eq!(m, vm_storage::SNAPSHOT_ON_OVERLAY);
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn revert_requires_a_stopped_vm_and_an_intact_snapshot() {
    let dir = common::tempdir();
    let mut cat = Catalog::open(dir.path()).unwrap();
    let vm = vm_with_disk(&mut cat, dir.path(), "checks");
    let a = take(&mut cat, &vm, "a");
    cat.with_tx(|tx| tx.set_vm_lifecycle(&vm.id, Lifecycle::Running))
        .unwrap();
    assert!(matches!(
        begin_revert(&mut cat, &vm, a),
        Err(StorageError::StateConflict(_))
    ));
    cat.with_tx(|tx| tx.set_vm_lifecycle(&vm.id, Lifecycle::Stopped))
        .unwrap();
    // Altered copy of the variables: refused before touching anything.
    let vars = cat.snapshot_vars(&vm.id, &a).unwrap().unwrap();
    let mut p = std::fs::metadata(&vars).unwrap().permissions();
    #[allow(clippy::permissions_set_readonly_false)]
    p.set_readonly(false);
    std::fs::set_permissions(&vars, p).unwrap();
    std::fs::write(&vars, b"tampered").unwrap();
    assert!(matches!(
        begin_revert(&mut cat, &vm, a),
        Err(StorageError::IntegrityMismatch { .. })
    ));
    // Tag gone from the disk: refused.
    FakeDisk::delete(&vm.disk(), &a.to_string());
    assert!(matches!(
        begin_revert(&mut cat, &vm, a),
        Err(StorageError::IntegrityMismatch { .. })
    ));
    assert!(cat.open_intents().unwrap().is_empty());
    assert!(tmp_empty(dir.path()));
}

/// Crash (the catalog dropped without any cleanup) at each point of each
/// operation, then reopened: the state is either fully before or fully
/// after the operation, as the disk says.
#[test]
fn crash_at_every_step_is_settled_from_the_disk() {
    #[derive(Debug, Clone, Copy)]
    enum Op {
        Take,
        Revert,
        Delete,
    }
    #[derive(Debug, Clone, Copy)]
    enum When {
        /// After the journal intent, before the disk operation.
        BeforeDisk,
        /// After the disk operation, before settling.
        AfterDisk,
        /// Files published or moved, transaction not committed.
        AfterPublish,
        /// Revert only: variables removed, snapshot copy not in place.
        VarsRemoved,
    }
    for op_kind in [Op::Take, Op::Revert, Op::Delete] {
        for when in [
            When::BeforeDisk,
            When::AfterDisk,
            When::AfterPublish,
            When::VarsRemoved,
        ] {
            if matches!(when, When::VarsRemoved) && !matches!(op_kind, Op::Revert) {
                continue;
            }
            let dir = common::tempdir();
            let root = dir.path();
            let (vm, a, b) = {
                let mut cat = Catalog::open(root).unwrap();
                let vm = vm_with_disk(&mut cat, root, "crash");
                let a = take(&mut cat, &vm, "a");
                FakeDisk::guest_write(&vm.disk());
                std::fs::write(vm.vars(), b"vars v2").unwrap();
                let b = take(&mut cat, &vm, "b");
                FakeDisk::guest_write(&vm.disk());
                std::fs::write(vm.vars(), b"vars v3").unwrap();
                let op: SnapshotOp = match op_kind {
                    Op::Take => begin_take(&mut cat, &vm, "c", false).unwrap(),
                    Op::Revert => begin_revert(&mut cat, &vm, a).unwrap(),
                    Op::Delete => cat
                        .begin_delete_snapshot(&vm.id, &a, &vm.disk_rel())
                        .unwrap(),
                };
                if !matches!(when, When::BeforeDisk) {
                    match op_kind {
                        Op::Take => FakeDisk::create(op.disk(), op.tag()),
                        Op::Revert => FakeDisk::apply(op.disk(), op.tag()),
                        Op::Delete => FakeDisk::delete(op.disk(), op.tag()),
                    }
                }
                let point = match when {
                    When::BeforeDisk | When::AfterDisk => FaultPoint::SnapshotBeforeSettle,
                    When::AfterPublish => FaultPoint::SnapshotAfterPublish,
                    When::VarsRemoved => FaultPoint::SnapshotRevertVarsRemoved,
                };
                cat.inject_fault(point, FaultAction::Crash);
                assert!(matches!(
                    cat.settle_snapshot(&op),
                    Err(StorageError::InjectedCrash(_))
                ));
                (vm, a, b)
            };
            let mut cat = Catalog::open(root).unwrap();
            let report = cat.open_report().unwrap().recovery.clone();
            assert_eq!(report.intents.len(), 1, "{op_kind:?} {when:?}: {report:?}");
            let done = !matches!(when, When::BeforeDisk);
            let expected = if done {
                RecoveryOutcome::RolledForward
            } else {
                RecoveryOutcome::RolledBack
            };
            assert_eq!(report.intents[0].outcome, expected, "{op_kind:?} {when:?}");
            assert!(cat.open_intents().unwrap().is_empty());
            let id = report.intents[0].intent_id;
            assert_eq!(
                cat.intent_state(id).unwrap(),
                if done {
                    IntentState::Committed
                } else {
                    IntentState::Aborted
                }
            );
            let list = cat.snapshot_list(&vm.id).unwrap();
            let names: Vec<&str> = list.snapshots.iter().map(|s| s.name.as_str()).collect();
            let tags = FakeDisk::tags(&vm.disk());
            // Catalog and disk agree.
            let mut row_tags: Vec<String> =
                list.snapshots.iter().map(|s| s.disk_tag.clone()).collect();
            let mut disk_tags = tags.clone();
            row_tags.sort();
            disk_tags.sort();
            assert_eq!(row_tags, disk_tags, "{op_kind:?} {when:?}");
            match (op_kind, done) {
                (Op::Take, true) => {
                    assert_eq!(names, ["a", "b", "c"]);
                    let c = list.snapshots[2].snapshot_id;
                    assert_eq!(list.current, Some(c));
                    assert_eq!(list.snapshots[2].parent_id, Some(b));
                    let vars = cat.snapshot_vars(&vm.id, &c).unwrap().unwrap();
                    assert_eq!(std::fs::read(vars).unwrap(), b"vars v3");
                }
                (Op::Take, false) | (Op::Revert, false) | (Op::Delete, false) => {
                    assert_eq!(names, ["a", "b"]);
                    assert_eq!(list.current, Some(b));
                    assert_eq!(std::fs::read(vm.vars()).unwrap(), b"vars v3");
                    assert_eq!(cat.read(|tx| tx.vm(&vm.id)).unwrap().revision, 1);
                }
                (Op::Revert, true) => {
                    assert_eq!(list.current, Some(a));
                    assert_eq!(std::fs::read(vm.vars()).unwrap(), b"vars v1");
                    assert_eq!(cat.read(|tx| tx.vm(&vm.id)).unwrap().revision, 2);
                }
                (Op::Delete, true) => {
                    assert_eq!(names, ["b"]);
                    assert_eq!(list.snapshots[0].parent_id, None);
                    assert_eq!(list.current, Some(b));
                    assert!(
                        !root
                            .join(vm_storage::CatalogLayout::snapshot_dir(&vm.id, &a).as_str())
                            .exists()
                    );
                }
            }
            assert!(tmp_empty(root), "{op_kind:?} {when:?}: tmp/ not empty");
            // The VM is free again for the next operation.
            delete(&mut cat, &vm, b);
        }
    }
}

#[test]
fn unreadable_disk_leaves_the_intent_for_review() {
    let dir = common::tempdir();
    let root = dir.path();
    let vm = {
        let mut cat = Catalog::open(root).unwrap();
        let vm = vm_with_disk(&mut cat, root, "garbled");
        let op = begin_take(&mut cat, &vm, "a", false).unwrap();
        cat.inject_fault(FaultPoint::SnapshotBeforeSettle, FaultAction::Crash);
        assert!(cat.settle_snapshot(&op).is_err());
        vm
    };
    std::fs::write(vm.disk(), b"not a qcow2 image any more").unwrap();
    let mut cat = Catalog::open(root).unwrap();
    let report = cat.open_report().unwrap().recovery.clone();
    assert_eq!(report.intents[0].outcome, RecoveryOutcome::NeedsReview);
    assert_eq!(cat.intents_needing_review().unwrap().len(), 1);
    assert_eq!(
        cat.read(|tx| tx.vm(&vm.id)).unwrap().lifecycle,
        Lifecycle::Recovering
    );
    // Once the disk is readable again the next reconciliation settles it.
    FakeDisk::new().save(&vm.disk());
    let r = cat.recover().unwrap();
    assert_eq!(r.intents[0].outcome, RecoveryOutcome::RolledBack);
    assert!(cat.open_intents().unwrap().is_empty());
}

#[test]
fn vm_deletion_removes_its_snapshots() {
    let dir = common::tempdir();
    let mut cat = Catalog::open(dir.path()).unwrap();
    let vm = vm_with_disk(&mut cat, dir.path(), "gone");
    let a = take(&mut cat, &vm, "a");
    let _b = take(&mut cat, &vm, "b");
    assert!(cat.snapshot_vars(&vm.id, &a).unwrap().unwrap().exists());
    cat.delete_vm_with_files(&vm.id, &[]).unwrap();
    assert!(!dir.path().join("vms").join(vm.id.to_string()).exists());
    assert_eq!(cat.read(|tx| tx.snapshot_count(&vm.id)).unwrap(), 0);
}

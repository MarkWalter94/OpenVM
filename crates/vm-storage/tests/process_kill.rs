//! F3 crash tests with a **real process kill**: the test binary re-runs
//! itself as a child process that performs a baseline operation and stops
//! at a critical point ([`FaultAction::Pause`] writes a marker and blocks);
//! the parent kills the child with `TerminateProcess` (`Child::kill`) while
//! it holds the catalog lock, the SQLite connection and the file handles,
//! then reopens the catalog: reconciliation must leave no loss, no orphan
//! file, no open intent. The child entry point is an ignored test that does
//! nothing unless `LIGHTVM_F3_KILL_CHILD` is set.

mod common;

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use vm_storage::{
    BaselineId, Catalog, CatalogLayout, CreateBaseline, CurrentStateFiles, FaultAction, FaultPoint,
    RestoreBaseline, VmId, qcow2_backing,
};

fn qcow2(backing: Option<&str>, payload: &[u8]) -> Vec<u8> {
    let mut h = vec![0u8; 4096];
    h[0..4].copy_from_slice(&[0x51, 0x46, 0x49, 0xfb]);
    h[4..8].copy_from_slice(&3u32.to_be_bytes());
    h[100..104].copy_from_slice(&104u32.to_be_bytes());
    if let Some(name) = backing {
        h[104..108].copy_from_slice(&0xe279_2acau32.to_be_bytes());
        h[108..112].copy_from_slice(&5u32.to_be_bytes());
        h[112..117].copy_from_slice(b"qcow2");
        h[8..16].copy_from_slice(&128u64.to_be_bytes());
        h[16..20].copy_from_slice(&(name.len() as u32).to_be_bytes());
        h[128..128 + name.len()].copy_from_slice(name.as_bytes());
    }
    h.extend_from_slice(payload);
    h
}

fn standalone(vm: &VmId) -> CurrentStateFiles {
    CurrentStateFiles {
        disk: CatalogLayout::primary_disk(vm),
        vars: Some(CatalogLayout::primary_vars(vm)),
    }
}

fn vm_dir(root: &Path, vm: &VmId) -> PathBuf {
    root.join("vms").join(vm.to_string())
}

/// Standalone content of a fake image (an overlay without data shows its base).
fn content(p: &Path) -> Vec<u8> {
    let b = std::fs::read(p).unwrap();
    let payload = b[4096..].to_vec();
    match qcow2_backing(p).unwrap() {
        Some((r, _)) if payload.is_empty() => content(
            &r.split('/')
                .fold(p.parent().unwrap().to_path_buf(), |a, c| a.join(c)),
        ),
        _ => payload,
    }
}

fn convert(src: &Path, dst: &Path) -> vm_storage::Result<u64> {
    std::fs::write(dst, qcow2(None, &content(src))).unwrap();
    Ok(1 << 30)
}

fn create_overlay(r: &str, _s: u64, dst: &Path) -> vm_storage::Result<()> {
    std::fs::write(dst, qcow2(Some(r), b"")).unwrap();
    Ok(())
}

fn create(cat: &mut Catalog, vm: &VmId, label: &str) -> vm_storage::Result<BaselineId> {
    let job = cat.begin_create_baseline(CreateBaseline {
        vm_id: *vm,
        label: label.into(),
        clean_shutdown: true,
        protected: false,
        standalone: standalone(vm),
    })?;
    cat.finish_create_baseline(job.run(convert))
        .map(|b| b.baseline_id)
}

fn restore(cat: &mut Catalog, vm: &VmId, b: &BaselineId) -> vm_storage::Result<()> {
    let job = cat.begin_restore(RestoreBaseline {
        vm_id: *vm,
        baseline_id: *b,
        standalone: standalone(vm),
    })?;
    cat.finish_restore(job.run(create_overlay)).map(|_| ())
}

/// Child process: performs the operation named in `LIGHTVM_F3_KILL_CHILD`
/// (`<point>|<catalog>|<vm>|<baseline>|<marker>`) and blocks at the point.
#[test]
#[ignore = "child process of the process-kill tests"]
fn kill_child() {
    let Ok(spec) = std::env::var("LIGHTVM_F3_KILL_CHILD") else {
        return;
    };
    let parts: Vec<&str> = spec.split('|').collect();
    let (point, root, vm, b, marker) = (parts[0], parts[1], parts[2], parts[3], parts[4]);
    let vm: VmId = vm.parse().unwrap();
    let b: BaselineId = b.parse().unwrap();
    let mut cat = Catalog::open(Path::new(root)).unwrap();
    let marker = PathBuf::from(marker);
    let pause = FaultAction::Pause(marker.clone());
    let _ = match point {
        "convert" => {
            // Killed while the image is being produced (worker step).
            let job = cat
                .begin_create_baseline(CreateBaseline {
                    vm_id: vm,
                    label: "killed".into(),
                    clean_shutdown: true,
                    protected: false,
                    standalone: standalone(&vm),
                })
                .unwrap();
            let _ = job.run(|_, dst| {
                std::fs::write(dst, qcow2(None, b"partial")).unwrap();
                std::fs::write(&marker, "convert").unwrap();
                loop {
                    std::thread::sleep(Duration::from_secs(3600));
                }
            });
            Ok(())
        }
        "create" => {
            cat.inject_fault(FaultPoint::CreateAfterDiskPublish, pause);
            create(&mut cat, &vm, "killed").map(|_| ())
        }
        "restore" => {
            cat.inject_fault(FaultPoint::RestoreAfterFirstDiscard, pause);
            restore(&mut cat, &vm, &b)
        }
        "delete" => {
            cat.inject_fault(FaultPoint::DeleteAfterFirstMove, pause);
            cat.delete_baseline_files(&vm, &b)
        }
        other => panic!("unknown point {other}"),
    };
    unreachable!("the child must block at the fault point");
}

struct Setup {
    dir: tempfile::TempDir,
    vm: VmId,
    b1: BaselineId,
    b2: BaselineId,
}

/// VM restored on b1 with changes in its overlay; b2 not in use.
fn setup() -> Setup {
    let dir = common::tempdir();
    let mut cat = Catalog::open(dir.path()).unwrap();
    let vm = common::new_vm(&mut cat, "vm");
    let d = vm_dir(dir.path(), &vm);
    std::fs::create_dir_all(d.join("firmware")).unwrap();
    std::fs::write(d.join("primary.qcow2"), qcow2(None, b"installed system")).unwrap();
    std::fs::write(d.join("firmware").join("primary.vars.fd"), b"VARS").unwrap();
    let b1 = create(&mut cat, &vm, "one").unwrap();
    let b2 = create(&mut cat, &vm, "two").unwrap();
    restore(&mut cat, &vm, &b1).unwrap();
    let ov = std::fs::read_dir(d.join("overlays"))
        .unwrap()
        .flatten()
        .next()
        .unwrap()
        .path();
    let r = qcow2_backing(&ov).unwrap().unwrap().0;
    std::fs::write(&ov, qcow2(Some(&r), b"changed since")).unwrap();
    Setup { dir, vm, b1, b2 }
}

fn files(p: &Path, out: &mut Vec<String>) {
    if let Ok(rd) = std::fs::read_dir(p) {
        for e in rd.flatten() {
            if e.path().is_dir() {
                files(&e.path(), out);
            } else {
                out.push(e.path().display().to_string());
            }
        }
    }
}

/// Runs the child, waits for its marker, kills it.
fn run_and_kill(s: &Setup, point: &str) -> Duration {
    let marker = s.dir.path().join(format!("marker-{point}"));
    let spec = format!(
        "{point}|{}|{}|{}|{}",
        s.dir.path().display(),
        s.vm,
        if point == "delete" { s.b2 } else { s.b1 },
        marker.display()
    );
    let mut child = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "kill_child", "--ignored", "--nocapture"])
        .env("LIGHTVM_F3_KILL_CHILD", spec)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let t0 = Instant::now();
    while !marker.exists() {
        assert!(
            t0.elapsed() < Duration::from_secs(60),
            "child did not reach {point}"
        );
        if let Some(st) = child.try_wait().unwrap() {
            panic!("child exited at {point}: {st:?}");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    // The child holds the lock: the catalog cannot be opened meanwhile.
    assert!(matches!(
        Catalog::open(s.dir.path()),
        Err(vm_storage::StorageError::CatalogLocked(_))
    ));
    let t = Instant::now();
    child.kill().unwrap();
    child.wait().unwrap();
    t.elapsed()
}

fn current_content(cat: &mut Catalog, s: &Setup) -> Vec<u8> {
    let rel = cat
        .read(|tx| {
            let cs = tx.current_state(&s.vm)?.unwrap();
            Ok(tx.disk(&cs.overlay_disk_id)?.managed_path)
        })
        .unwrap();
    content(&cat.layout().resolve(&rel).unwrap())
}

fn assert_clean(cat: &mut Catalog, s: &Setup) {
    assert!(cat.open_intents().unwrap().is_empty(), "open intents");
    assert_eq!(
        std::fs::read_dir(s.dir.path().join("tmp")).unwrap().count(),
        0,
        "tmp/ not empty"
    );
    // Every file under the VM directory is registered.
    let mut reg: Vec<String> = cat
        .read(|tx| {
            let mut v = Vec::new();
            for d in tx.disks_of_vm(&s.vm)? {
                v.push(d.managed_path.to_string());
            }
            for b in tx.baselines_of_vm(&s.vm)? {
                if let Some(f) = b.vars_fw_id {
                    v.push(tx.firmware(&f)?.managed_path.to_string());
                }
                v.push(CatalogLayout::baseline_manifest(&s.vm, &b.baseline_id).to_string());
            }
            if let Some(cs) = tx.current_state(&s.vm)?
                && let Some(f) = cs.vars_fw_id
            {
                v.push(tx.firmware(&f)?.managed_path.to_string());
            }
            Ok(v)
        })
        .unwrap();
    reg.push(CatalogLayout::vm_manifest(&s.vm).to_string());
    let root = s.dir.path().display().to_string();
    let mut found = Vec::new();
    files(&vm_dir(s.dir.path(), &s.vm), &mut found);
    for f in found {
        let rel = f[root.len() + 1..].replace('\\', "/");
        assert!(reg.contains(&rel), "orphan {rel}");
    }
    assert!(
        cat.verify_baseline_manifest(&s.b1, true)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn process_killed_during_convert_create_restore_and_delete() {
    for point in ["convert", "create", "restore", "delete"] {
        let s = setup();
        let before = {
            let mut cat = Catalog::open(s.dir.path()).unwrap();
            (
                common::base_hash(&mut cat, &s.b1),
                current_content(&mut cat, &s),
            )
        };
        let kill = run_and_kill(&s, point);
        let mut cat = Catalog::open(s.dir.path()).unwrap();
        let report = cat.open_report().unwrap().recovery.clone();
        eprintln!(
            "{point}: killed in {kill:?}; reconciliation: {:?}",
            report
                .intents
                .iter()
                .map(|i| (i.kind, i.outcome))
                .collect::<Vec<_>>()
        );
        assert_eq!(
            common::base_hash(&mut cat, &s.b1),
            before.0,
            "{point}: base"
        );
        let ids: Vec<BaselineId> = cat
            .read(|tx| {
                Ok(tx
                    .baselines_of_vm(&s.vm)?
                    .into_iter()
                    .map(|b| b.baseline_id)
                    .collect())
            })
            .unwrap();
        match point {
            // Creation interrupted: no new baseline.
            "convert" | "create" => assert_eq!(ids, vec![s.b1, s.b2], "{point}"),
            // Deletion rolled forward once a file was moved.
            "delete" => assert_eq!(ids, vec![s.b1], "{point}"),
            _ => assert_eq!(ids, vec![s.b1, s.b2], "{point}"),
        }
        // Restore interrupted during the discard: the old state is back.
        assert_eq!(current_content(&mut cat, &s), before.1, "{point}: state");
        assert_clean(&mut cat, &s);
        // Nothing stays busy: a full restore succeeds.
        restore(&mut cat, &s.vm, &s.b1).unwrap();
        assert_eq!(current_content(&mut cat, &s), b"installed system");
    }
}

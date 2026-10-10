//! Shared support for integration tests (temporary directories, fake data).
#![allow(dead_code, unreachable_pub)]

use std::path::Path;

use vm_storage::{BaselineId, VmId};
use vm_storage::{Catalog, NewVm, PromoteBaseline, Result, RunRecord, StorageError};

pub fn tempdir() -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix("lightvm-storage-test-")
        .tempdir()
        .expect("temporary directory")
}

pub fn new_vm(cat: &mut Catalog, name: &str) -> VmId {
    cat.with_tx(|tx| {
        tx.insert_vm(&NewVm {
            name: name.into(),
            guest_os: "ubuntu-24.04".into(),
            config_version: 1,
            config: serde_json::json!({"cpus": 2, "memory_mib": 2048, "network": "offline"}),
        })
    })
    .expect("VM creation")
}

pub fn write_bytes(path: &Path, bytes: &[u8]) -> Result<()> {
    std::fs::write(path, bytes).map_err(|e| StorageError::Io {
        path: path.to_path_buf(),
        source: e,
    })
}

/// Baseline with a fake base disk and vars.
pub fn baseline(cat: &mut Catalog, vm: &VmId, protected: bool) -> BaselineId {
    cat.promote_baseline(
        &PromoteBaseline {
            vm_id: *vm,
            label: "clean install".into(),
            clean_shutdown: true,
            protected,
            virtual_size_bytes: 20 * 1024 * 1024 * 1024,
        },
        |p| write_bytes(p, b"QFI\xfb fake immutable base"),
        Some(Box::new(|p: &Path| write_bytes(p, b"fake OVMF_VARS"))),
    )
    .expect("baseline promotion")
}

pub fn run_from(cat: &mut Catalog, vm: &VmId, b: &BaselineId) -> RunRecord {
    cat.prepare_run_from_baseline(vm, b, |tmp, base| {
        assert!(base.is_file(), "the base must exist");
        write_bytes(tmp, b"QFI\xfb fake overlay")
    })
    .expect("run preparation")
}

pub fn base_hash(cat: &mut Catalog, b: &BaselineId) -> String {
    let rel = cat
        .read(|tx| Ok(tx.disk(&tx.baseline(b)?.base_disk_id)?.managed_path))
        .unwrap();
    vm_storage::sha256_file(&cat.layout().resolve(&rel).unwrap()).unwrap()
}

/// Creates an NTFS junction (`mklink /J`, no privileges required).
/// Returns `false` if the environment does not allow it.
pub fn make_junction(link: &Path, target: &Path) -> bool {
    if !cfg!(windows) {
        return false;
    }
    std::process::Command::new("cmd")
        .arg("/C")
        .arg("mklink")
        .arg("/J")
        .arg(link)
        .arg(target)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

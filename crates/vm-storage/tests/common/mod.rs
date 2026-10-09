//! Supporto comune ai test di integrazione (directory temporanee, dati finti).
#![allow(dead_code)]

use std::path::Path;

use vm_storage::compat::{BaselineId, VmId};
use vm_storage::{Catalog, NewVm, PromoteBaseline, Result, RunRecord, StorageError};

pub fn tempdir() -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix("lightvm-storage-test-")
        .tempdir()
        .expect("directory temporanea")
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
    .expect("creazione VM")
}

pub fn write_bytes(path: &Path, bytes: &[u8]) -> Result<()> {
    std::fs::write(path, bytes).map_err(|e| StorageError::Io {
        path: path.to_path_buf(),
        source: e,
    })
}

/// Baseline con disco base e vars finti.
pub fn baseline(cat: &mut Catalog, vm: &VmId, protected: bool) -> BaselineId {
    cat.promote_baseline(
        &PromoteBaseline {
            vm_id: *vm,
            label: "installazione pulita".into(),
            clean_shutdown: true,
            protected,
            virtual_size_bytes: 20 * 1024 * 1024 * 1024,
        },
        |p| write_bytes(p, b"QFI\xfb base finta immutabile"),
        Some(Box::new(|p: &Path| write_bytes(p, b"OVMF_VARS finto"))),
    )
    .expect("promozione baseline")
}

pub fn run_from(cat: &mut Catalog, vm: &VmId, b: &BaselineId) -> RunRecord {
    cat.prepare_run_from_baseline(vm, b, |tmp, base| {
        assert!(base.is_file(), "la base deve esistere");
        write_bytes(tmp, b"QFI\xfb overlay finto")
    })
    .expect("preparazione esecuzione")
}

pub fn base_hash(cat: &mut Catalog, b: &BaselineId) -> String {
    let rel = cat
        .read(|tx| Ok(tx.disk(&tx.baseline(b)?.base_disk_id)?.managed_path))
        .unwrap();
    vm_storage::sha256_file(&cat.layout().resolve(&rel).unwrap()).unwrap()
}

/// Crea una junction NTFS (`mklink /J`, nessun privilegio richiesto).
/// Restituisce `false` se l'ambiente non lo consente.
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

//! Budget storage applicativo e manifest versionati.

mod common;

use vm_storage::compat::{ArtifactId, MediaId};
use vm_storage::{
    ArtifactKind, ArtifactRecord, Catalog, CatalogLayout, Divergence, Enforcement, MediaKind,
    MediaRecord, MediaStatus, StorageError, now_rfc3339,
};

#[test]
fn usage_is_accounted_per_category() {
    let dir = common::tempdir();
    let mut cat = Catalog::open(dir.path()).unwrap();
    let vm = common::new_vm(&mut cat, "vm");
    let other = common::new_vm(&mut cat, "altra");
    let b = common::baseline(&mut cat, &vm, false);
    let run = common::run_from(&mut cat, &vm, &b);
    let media = MediaId::new_v7();
    cat.with_tx(|tx| {
        for (kind, size) in [(ArtifactKind::Screenshot, 1000), (ArtifactKind::Log, 300)] {
            let id = ArtifactId::new_v7();
            tx.insert_artifact(&ArtifactRecord {
                artifact_id: id,
                run_id: run.run_id,
                kind,
                managed_path: CatalogLayout::run_artifacts(&run.run_id).join(&format!("{id}.bin"))?,
                sha256: "0".repeat(64),
                size_bytes: size,
                created_at: now_rfc3339(),
                retain_until: None,
            })?;
        }
        tx.insert_media(&MediaRecord {
            media_id: media,
            sha256: "1".repeat(64),
            size_bytes: 5000,
            kind: MediaKind::Data,
            declared_origin: "fixture".into(),
            approved_at: now_rfc3339(),
            managed_path: CatalogLayout::media_file(&media, "iso")?,
            status: MediaStatus::Approved,
        })
    })
    .unwrap();
    let u = cat.storage_usage(Some(&vm)).unwrap();
    assert_eq!(u.artifacts, 1000);
    assert_eq!(u.logs, 300);
    assert!(u.baselines > 0 && u.overlays > 0 && u.run_firmware > 0);
    assert_eq!(u.media, 0, "media solo nel totale del catalogo");
    let all = cat.storage_usage(None).unwrap();
    assert_eq!(all.media, 5000);
    assert!(all.metadata > 0, "database contabilizzato");
    assert_eq!(cat.storage_usage(Some(&other)).unwrap().total(), 0);
    // Misura dai file.
    let ov = run.overlay_disk_id.unwrap();
    let managed = cat.read(|tx| tx.disk(&ov)).unwrap().managed_path;
    let p = cat.layout().resolve(&managed).unwrap();
    std::fs::write(&p, vec![0u8; 4096]).unwrap();
    cat.refresh_disk_sizes().unwrap();
    assert_eq!(cat.read(|tx| tx.disk(&ov)).unwrap().allocated_bytes, 4096);
}

#[test]
fn new_run_is_refused_over_budget_or_reserve() {
    let dir = common::tempdir();
    let mut cat = Catalog::open(dir.path()).unwrap();
    let vm = common::new_vm(&mut cat, "vm");
    let b = common::baseline(&mut cat, &vm, false);
    common::run_from(&mut cat, &vm, &b);
    let gib = 1u64 << 30;

    // Nessun budget dichiarato: vale solo la riserva host (assente = 0).
    cat.check_new_run(&vm, gib, 10 * gib, Enforcement::ApplicationBudget)
        .unwrap();

    cat.set_global_budget(4 * gib, 2 * gib).unwrap();
    cat.set_vm_budget(&vm, 2 * gib).unwrap();
    let ok = cat
        .check_new_run(&vm, gib, 10 * gib, Enforcement::ApplicationBudget)
        .unwrap();
    assert_eq!(ok.config.vm_bytes, Some(2 * gib));

    let err = cat
        .check_new_run(&vm, 3 * gib, 100 * gib, Enforcement::ApplicationBudget)
        .unwrap_err();
    assert!(matches!(&err, StorageError::ResourceLimit(m) if m.contains("VM")), "{err:?}");
    assert_eq!(err.public_code(), Some("resource_limit"));

    cat.set_vm_budget(&vm, 100 * gib).unwrap();
    let err = cat
        .check_new_run(&vm, 5 * gib, 100 * gib, Enforcement::ApplicationBudget)
        .unwrap_err();
    assert!(matches!(&err, StorageError::ResourceLimit(m) if m.contains("catalogo")));

    let err = cat
        .check_new_run(&vm, gib, 2 * gib, Enforcement::ApplicationBudget)
        .unwrap_err();
    assert!(matches!(&err, StorageError::ResourceLimit(m) if m.contains("riserva")));

    let err = cat
        .check_new_run(&vm, 1, 100 * gib, Enforcement::OsHardQuota)
        .unwrap_err();
    assert!(matches!(err, StorageError::Unsupported(_)));
    assert_eq!(err.public_code(), Some("unsupported"));
}

#[test]
fn vm_manifest_divergences_are_detected() {
    let dir = common::tempdir();
    let mut cat = Catalog::open(dir.path()).unwrap();
    let vm = common::new_vm(&mut cat, "vm");
    assert!(cat
        .verify_vm_manifest(&vm)
        .unwrap()
        .contains(&Divergence::ManifestMissing));
    cat.write_vm_manifest(&vm).unwrap();
    assert!(cat.verify_vm_manifest(&vm).unwrap().is_empty());
    let b = common::baseline(&mut cat, &vm, false);
    // `promote_baseline` riscrive il manifest della VM.
    assert!(cat.verify_vm_manifest(&vm).unwrap().is_empty());

    // Modifica nel DB non riportata nel manifest.
    cat.with_tx(|tx| tx.update_vm_config(&vm, 1, 1, &serde_json::json!({"cpus": 8})))
        .unwrap();
    let d = cat.verify_vm_manifest(&vm).unwrap();
    assert!(d.contains(&Divergence::FieldMismatch("config")), "{d:?}");
    assert!(d.contains(&Divergence::FieldMismatch("config_revision")));
    cat.write_vm_manifest(&vm).unwrap();
    assert!(cat.verify_vm_manifest(&vm).unwrap().is_empty());

    // Modifica del file.
    let p = dir.path().join(CatalogLayout::vm_manifest(&vm).as_str());
    let text = std::fs::read_to_string(&p).unwrap().replace("\"vm\"", "\"altro\"");
    std::fs::write(&p, text).unwrap();
    let d = cat.verify_vm_manifest(&vm).unwrap();
    assert!(d.iter().any(|x| matches!(x, Divergence::HashMismatch { .. })), "{d:?}");
    assert!(d.contains(&Divergence::FieldMismatch("name")));
    let _ = b;
}

#[test]
#[allow(clippy::permissions_set_readonly_false)]
fn baseline_manifest_detects_altered_base() {
    let dir = common::tempdir();
    let mut cat = Catalog::open(dir.path()).unwrap();
    let vm = common::new_vm(&mut cat, "vm");
    let b = common::baseline(&mut cat, &vm, false);
    assert!(cat.verify_baseline_manifest(&b, true).unwrap().is_empty());
    let m: vm_storage::BaselineManifest = serde_json::from_slice(
        &std::fs::read(dir.path().join(CatalogLayout::baseline_manifest(&vm, &b).as_str()))
            .unwrap(),
    )
    .unwrap();
    assert_eq!(m.format_version, vm_storage::MANIFEST_FORMAT_VERSION);
    assert!(m.created_powered_off);
    assert!(m.vars.is_some());

    // Alterazione della base (rimuovendo prima la sola lettura).
    let p = dir.path().join(CatalogLayout::baseline_disk(&vm, &b).as_str());
    let mut perm = std::fs::metadata(&p).unwrap().permissions();
    assert!(perm.readonly());
    assert!(std::fs::write(&p, b"scrittura").is_err(), "base in sola lettura");
    perm.set_readonly(false);
    std::fs::set_permissions(&p, perm).unwrap();
    std::fs::write(&p, b"alterata").unwrap();
    assert!(cat.verify_baseline_manifest(&b, false).unwrap().is_empty());
    let d = cat.verify_baseline_manifest(&b, true).unwrap();
    assert!(matches!(d.as_slice(), [Divergence::DataHashMismatch(_)]), "{d:?}");
}

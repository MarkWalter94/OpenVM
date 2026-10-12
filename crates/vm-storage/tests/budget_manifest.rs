//! Application storage budget and versioned manifests.

mod common;

use vm_storage::{ArtifactId, MediaId};
use vm_storage::{
    ArtifactKind, ArtifactRecord, Catalog, CatalogLayout, Divergence, Enforcement, MediaKind,
    MediaRecord, MediaStatus, StorageError, now_rfc3339,
};

#[test]
fn usage_is_accounted_per_category() {
    let dir = common::tempdir();
    let mut cat = Catalog::open(dir.path()).unwrap();
    let vm = common::new_vm(&mut cat, "vm");
    let other = common::new_vm(&mut cat, "other");
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
                managed_path: CatalogLayout::run_artifacts(&run.run_id)
                    .join(&format!("{id}.bin"))?,
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
            detected_guest: None,
            guest_examined: false,
        })
    })
    .unwrap();
    let u = cat.storage_usage(Some(&vm)).unwrap();
    assert_eq!(u.artifacts, 1000);
    assert_eq!(u.logs, 300);
    assert!(u.baselines > 0 && u.overlays > 0 && u.run_firmware > 0);
    assert_eq!(u.media, 0, "media only in the catalog total");
    let all = cat.storage_usage(None).unwrap();
    assert_eq!(all.media, 5000);
    assert!(all.metadata > 0, "database accounted for");
    assert_eq!(cat.storage_usage(Some(&other)).unwrap().total(), 0);
    // Measured from the files.
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

    // No budget declared: only the host reserve applies (absent = 0).
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
    assert!(
        matches!(&err, StorageError::ResourceLimit(m) if m.contains("VM")),
        "{err:?}"
    );
    assert_eq!(err.public_code(), Some("resource_limit"));

    cat.set_vm_budget(&vm, 100 * gib).unwrap();
    let err = cat
        .check_new_run(&vm, 5 * gib, 100 * gib, Enforcement::ApplicationBudget)
        .unwrap_err();
    assert!(matches!(&err, StorageError::ResourceLimit(m) if m.contains("catalog")));

    let err = cat
        .check_new_run(&vm, gib, 2 * gib, Enforcement::ApplicationBudget)
        .unwrap_err();
    assert!(matches!(&err, StorageError::ResourceLimit(m) if m.contains("reserve")));

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
    assert!(
        cat.verify_vm_manifest(&vm)
            .unwrap()
            .contains(&Divergence::ManifestMissing)
    );
    cat.write_vm_manifest(&vm).unwrap();
    assert!(cat.verify_vm_manifest(&vm).unwrap().is_empty());
    let b = common::baseline(&mut cat, &vm, false);
    // `promote_baseline` rewrites the VM manifest.
    assert!(cat.verify_vm_manifest(&vm).unwrap().is_empty());

    // Change in the DB not reflected in the manifest.
    cat.with_tx(|tx| tx.update_vm_config(&vm, 1, 1, &serde_json::json!({"cpus": 8})))
        .unwrap();
    let d = cat.verify_vm_manifest(&vm).unwrap();
    assert!(d.contains(&Divergence::FieldMismatch("config")), "{d:?}");
    assert!(d.contains(&Divergence::FieldMismatch("config_revision")));
    cat.write_vm_manifest(&vm).unwrap();
    assert!(cat.verify_vm_manifest(&vm).unwrap().is_empty());

    // Change to the file.
    let p = dir.path().join(CatalogLayout::vm_manifest(&vm).as_str());
    let text = std::fs::read_to_string(&p)
        .unwrap()
        .replace("\"vm\"", "\"other\"");
    std::fs::write(&p, text).unwrap();
    let d = cat.verify_vm_manifest(&vm).unwrap();
    assert!(
        d.iter()
            .any(|x| matches!(x, Divergence::HashMismatch { .. })),
        "{d:?}"
    );
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
        &std::fs::read(
            dir.path()
                .join(CatalogLayout::baseline_manifest(&vm, &b).as_str()),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(m.format_version, vm_storage::MANIFEST_FORMAT_VERSION);
    assert!(m.created_powered_off);
    assert!(m.vars.is_some());

    // Tampering with the base (removing the read-only attribute first).
    let p = dir
        .path()
        .join(CatalogLayout::baseline_disk(&vm, &b).as_str());
    let mut perm = std::fs::metadata(&p).unwrap().permissions();
    assert!(perm.readonly());
    assert!(std::fs::write(&p, b"write").is_err(), "base is read-only");
    perm.set_readonly(false);
    std::fs::set_permissions(&p, perm).unwrap();
    std::fs::write(&p, b"tampered").unwrap();
    assert!(cat.verify_baseline_manifest(&b, false).unwrap().is_empty());
    let d = cat.verify_baseline_manifest(&b, true).unwrap();
    assert!(
        matches!(d.as_slice(), [Divergence::DataHashMismatch(_)]),
        "{d:?}"
    );
}

#[test]
fn budget_manifest_reports_usage_per_vm_baseline_and_run() {
    let dir = common::tempdir();
    let mut cat = Catalog::open(dir.path()).unwrap();
    let vm = common::new_vm(&mut cat, "vm");
    let b = common::baseline(&mut cat, &vm, false);
    let run = common::run_from(&mut cat, &vm, &b);
    // Overlay grown during the run, then measured.
    let ov = run.overlay_disk_id.unwrap();
    let managed = cat.read(|tx| tx.disk(&ov)).unwrap().managed_path;
    std::fs::write(cat.layout().resolve(&managed).unwrap(), vec![0u8; 8192]).unwrap();
    cat.refresh_disk_sizes().unwrap();
    // VM budget declared below current usage: exceeded.
    cat.set_vm_budget(&vm, 1000).unwrap();
    cat.set_global_budget(1 << 30, 4096).unwrap();

    let m = cat.write_budget_manifest().unwrap();
    assert_eq!(m.kind, "budget");
    assert_eq!(m.enforcement, "application_budget");
    assert_eq!(m.global_bytes, Some(1 << 30));
    assert_eq!(m.host_reserve_bytes, 4096);
    assert!(!m.catalog_exceeded);
    assert_eq!(m.vms.len(), 1);
    let v = &m.vms[0];
    assert_eq!(v.vm_id, vm);
    assert_eq!(v.budget_bytes, Some(1000));
    assert!(v.exceeded, "VM budget exceeded: {v:?}");
    assert_eq!(v.total_bytes, v.usage.total());
    assert_eq!(v.baselines.len(), 1);
    assert_eq!(v.baselines[0].baseline_id, b);
    assert!(v.baselines[0].disk_bytes > 0 && v.baselines[0].vars_bytes > 0);
    assert_eq!(v.runs.len(), 1);
    let r = &v.runs[0];
    assert_eq!(r.run_id, run.run_id);
    assert!(!r.ended);
    assert_eq!(r.overlay_bytes, 8192);
    assert!(r.firmware_bytes > 0);
    assert_eq!(
        r.total_bytes,
        r.overlay_bytes + r.firmware_bytes + r.artifact_bytes + r.log_bytes
    );
    // Per-category sum consistent with the detail rows.
    assert_eq!(v.usage.overlays, r.overlay_bytes);
    assert_eq!(v.usage.baselines, v.baselines[0].total_bytes);

    // File written atomically, with recorded hash and readable content.
    let path = dir.path().join(vm_storage::BUDGET_MANIFEST_FILE);
    let bytes = std::fs::read(&path).unwrap();
    let back: vm_storage::BudgetManifest = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(back, m);
    drop(cat);
    let conn = vm_storage::migrate::open_connection(&dir.path().join(vm_storage::DB_FILE)).unwrap();
    let recorded: String = conn
        .query_row(
            "SELECT value FROM catalog_meta WHERE key = 'budget_manifest_sha256'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(recorded, vm_storage::sha256_file(&path).unwrap());
}

#[test]
fn run_over_budget_is_refused_before_any_file_or_intent() {
    let dir = common::tempdir();
    let mut cat = Catalog::open(dir.path()).unwrap();
    let vm = common::new_vm(&mut cat, "vm");
    let b = common::baseline(&mut cat, &vm, false);
    let used = cat.storage_usage(Some(&vm)).unwrap().total();
    cat.set_vm_budget(&vm, used + 100).unwrap();
    let req = |requested_bytes| vm_storage::RunBudgetRequest {
        requested_bytes,
        host_free_bytes: 1 << 40,
        enforcement: Enforcement::ApplicationBudget,
    };
    let mut called = false;
    let err = cat
        .prepare_run_within_budget(&vm, &b, &req(1000), |t, _| {
            called = true;
            common::write_bytes(t, b"overlay")
        })
        .unwrap_err();
    assert!(
        matches!(&err, StorageError::ResourceLimit(m) if m.contains("VM")),
        "{err:?}"
    );
    assert!(!called, "the overlay producer was not invoked");
    assert!(cat.open_intents().unwrap().is_empty());
    let overlays = dir.path().join("vms").join(vm.to_string()).join("overlays");
    assert_eq!(
        std::fs::read_dir(&overlays).map(|i| i.count()).unwrap_or(0),
        0
    );
    assert!(cat.budget_manifest().unwrap().vms[0].runs.is_empty());

    // Within budget the run is prepared and the check is returned.
    let (run, check) = cat
        .prepare_run_within_budget(&vm, &b, &req(50), |t, _| common::write_bytes(t, b"o"))
        .unwrap();
    assert_eq!(check.requested_bytes, 50);
    assert_eq!(check.config.vm_bytes, Some(used + 100));
    assert_eq!(cat.read(|tx| tx.run(&run.run_id)).unwrap().vm_id, vm);
}

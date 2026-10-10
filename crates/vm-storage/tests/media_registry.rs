//! Approved media registry (ADR-0008; ROADMAP 7.4, "File" and "Host" rows):
//! read-only managed copy, SHA-256 of the copied bytes, `CREATE_NEW`,
//! no overwriting, source replaced after approval.

mod common;

use sha2::{Digest, Sha256};
use vm_storage::{
    Catalog, CatalogLayout, IntentKind, IntentSpec, IntentState, MediaId, MediaKind, MediaSlot,
    MediaStatus, PathRejection, StorageError, TargetKind,
};

fn sha_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

#[test]
fn approved_media_is_a_readonly_managed_copy_with_hash() {
    let dir = common::tempdir();
    let src_dir = common::tempdir();
    let src = src_dir.path().join("ubuntu.iso");
    let content = b"CD001 fake iso for the registry".repeat(5000);
    std::fs::write(&src, &content).unwrap();
    let mut cat = Catalog::open(dir.path()).unwrap();

    let m = cat
        .approve_media(
            &src,
            MediaKind::Iso,
            "ubuntu-24.04.3 from the official site",
        )
        .unwrap();
    assert_eq!(m.sha256, sha_hex(&content));
    assert_eq!(m.size_bytes, content.len() as u64);
    assert_eq!(m.status, MediaStatus::Approved);
    assert_eq!(
        m.managed_path,
        CatalogLayout::media_file(&m.media_id, "iso").unwrap()
    );
    let copy = cat.layout().resolve(&m.managed_path).unwrap();
    assert_eq!(std::fs::read(&copy).unwrap(), content);
    assert!(std::fs::metadata(&copy).unwrap().permissions().readonly());
    assert!(cat.open_intents().unwrap().is_empty());
    assert_eq!(cat.read(|tx| tx.media(&m.media_id)).unwrap(), m);
    assert_eq!(
        std::fs::read_dir(dir.path().join("tmp")).unwrap().count(),
        0,
        "no leftover temporary"
    );

    // ISO replaced after approval: the managed copy does not change.
    std::fs::write(&src, b"replaced content").unwrap();
    let check = cat.verify_media(&m.media_id).unwrap();
    assert_eq!(check.record.sha256, sha_hex(&content));
    assert!(!check.readonly_restored);

    // Can be attached to a VM as installation media.
    let vm = common::new_vm(&mut cat, "vm");
    cat.with_tx(|tx| tx.attach_media(&vm, MediaSlot::Install, &m.media_id))
        .unwrap();
}

#[test]
fn altered_managed_copy_is_detected() {
    let dir = common::tempdir();
    let src_dir = common::tempdir();
    let src = src_dir.path().join("data.iso");
    std::fs::write(&src, b"original data").unwrap();
    let mut cat = Catalog::open(dir.path()).unwrap();
    let m = cat.approve_media(&src, MediaKind::Data, "fixture").unwrap();
    let copy = cat.layout().resolve(&m.managed_path).unwrap();
    #[allow(clippy::permissions_set_readonly_false)]
    {
        let mut p = std::fs::metadata(&copy).unwrap().permissions();
        p.set_readonly(false);
        std::fs::set_permissions(&copy, p).unwrap();
    }
    std::fs::write(&copy, b"altered data").unwrap();
    let err = cat.verify_media(&m.media_id).unwrap_err();
    assert!(
        matches!(err, StorageError::IntegrityMismatch { .. }),
        "{err:?}"
    );

    // Copy removed: media `missing`.
    std::fs::remove_file(&copy).unwrap();
    let err = cat.verify_media(&m.media_id).unwrap_err();
    assert!(matches!(err, StorageError::NotFound { .. }), "{err:?}");
    assert_eq!(
        cat.read(|tx| tx.media(&m.media_id)).unwrap().status,
        MediaStatus::Missing
    );
}

#[test]
fn source_that_is_a_directory_or_link_is_refused() {
    let dir = common::tempdir();
    let src_dir = common::tempdir();
    let mut cat = Catalog::open(dir.path()).unwrap();
    // A directory instead of a file.
    let err = cat
        .approve_media(src_dir.path(), MediaKind::Iso, "x")
        .unwrap_err();
    assert!(
        matches!(
            err,
            StorageError::InvalidPath {
                reason: PathRejection::Link,
                ..
            } | StorageError::Io { .. }
        ),
        "{err:?}"
    );
    // Symbolic link to a file (if allowed without privileges).
    let target = src_dir.path().join("real.iso");
    std::fs::write(&target, b"iso").unwrap();
    let link = src_dir.path().join("link.iso");
    #[cfg(windows)]
    let made = std::os::windows::fs::symlink_file(&target, &link).is_ok();
    #[cfg(unix)]
    let made = std::os::unix::fs::symlink(&target, &link).is_ok();
    if made {
        let err = cat.approve_media(&link, MediaKind::Iso, "x").unwrap_err();
        assert!(
            matches!(
                err,
                StorageError::InvalidPath {
                    reason: PathRejection::Link,
                    ..
                }
            ),
            "{err:?}"
        );
    } else {
        eprintln!("symlink not allowed (missing privilege): part skipped");
    }
    // Declared origin with control characters.
    let err = cat
        .approve_media(&target, MediaKind::Iso, "line1\nline2")
        .unwrap_err();
    assert!(matches!(err, StorageError::Constraint(_)), "{err:?}");
    // No file or intent left behind by the refusals.
    assert_eq!(
        std::fs::read_dir(dir.path().join("media")).unwrap().count(),
        0
    );
    assert!(cat.open_intents().unwrap().is_empty());
}

#[test]
fn destination_appearing_before_publication_is_never_overwritten() {
    let dir = common::tempdir();
    let mut cat = Catalog::open(dir.path()).unwrap();
    let id = MediaId::new_v7();
    let rel = CatalogLayout::media_file(&id, "iso").unwrap();
    let i = cat
        .begin_intent(IntentSpec {
            kind: IntentKind::ImportMedia,
            vm_id: None,
            operation_id: None,
            target_kind: TargetKind::Media,
            target_id: Some(id.to_string()),
            final_path: Some(rel.clone()),
            read_only: true,
        })
        .unwrap();
    common::write_bytes(&i.temp_path, b"managed copy").unwrap();
    // Another actor creates the destination between `begin_intent` and publication.
    let dest = cat.layout().resolve(&rel).unwrap();
    std::fs::write(&dest, b"foreign file").unwrap();
    let err = cat.mark_applied(&i).unwrap_err();
    assert!(matches!(err, StorageError::StateConflict(_)), "{err:?}");
    assert_eq!(std::fs::read(&dest).unwrap(), b"foreign file");
    assert_eq!(cat.intent_state(i.id).unwrap(), IntentState::Pending);
    cat.abort(&i).unwrap();
    assert!(!i.temp_path.exists());
    assert_eq!(
        std::fs::read(&dest).unwrap(),
        b"foreign file",
        "the rollback does not touch a row-less file it did not create"
    );
}

#[cfg(windows)]
#[test]
fn source_open_by_a_writer_is_refused() {
    use std::os::windows::fs::OpenOptionsExt;
    let dir = common::tempdir();
    let src_dir = common::tempdir();
    let src = src_dir.path().join("being-written.iso");
    std::fs::write(&src, b"iso being written").unwrap();
    let _writer = std::fs::OpenOptions::new()
        .write(true)
        .share_mode(0x1 | 0x2 | 0x4)
        .open(&src)
        .unwrap();
    let mut cat = Catalog::open(dir.path()).unwrap();
    let err = cat.approve_media(&src, MediaKind::Iso, "x").unwrap_err();
    assert!(matches!(err, StorageError::VmBusy(_)), "{err:?}");
}

#[test]
fn checked_approval_enforces_size_and_optical_signature_on_the_handle() {
    use vm_storage::{MediaLimits, NOT_OPTICAL_IMAGE};
    let dir = common::tempdir();
    let src_dir = common::tempdir();
    let mut cat = Catalog::open(dir.path()).unwrap();
    let image = |name: &str, id: &[u8], len: usize| {
        let mut b = vec![0u8; len];
        let o = 16 * 2048 + 1;
        if len >= o + id.len() {
            b[o..o + id.len()].copy_from_slice(id);
        }
        let p = src_dir.path().join(name);
        std::fs::write(&p, b).unwrap();
        p
    };
    let strict = MediaLimits {
        max_bytes: 64 * 1024,
        optical_signature: true,
    };
    // ISO 9660 and UDF accepted.
    for (name, id) in [("a.iso", b"CD001"), ("b.iso", b"BEA01")] {
        let m = cat
            .approve_media_checked(&image(name, id, 40 * 1024), MediaKind::Data, "x", strict)
            .unwrap();
        assert_eq!(m.size_bytes, 40 * 1024);
    }
    // Wrong signature and file too short: refused, nothing left behind.
    for p in [
        image("zip.iso", b"PK\x03\x04\x00", 40 * 1024),
        image("short.iso", b"", 1024),
    ] {
        match cat.approve_media_checked(&p, MediaKind::Data, "x", strict) {
            Err(StorageError::Unsupported(m)) => assert_eq!(m, NOT_OPTICAL_IMAGE),
            other => panic!("{other:?}"),
        }
    }
    // Over the size limit: refused before copying.
    let big = image("big.iso", b"CD001", 65 * 1024);
    assert!(matches!(
        cat.approve_media_checked(&big, MediaKind::Iso, "x", strict),
        Err(StorageError::ResourceLimit(_))
    ));
    assert!(cat.open_intents().unwrap().is_empty());
    assert_eq!(
        std::fs::read_dir(dir.path().join("tmp")).unwrap().count(),
        0
    );
    let media = std::fs::read_dir(dir.path().join("media")).unwrap().count();
    assert_eq!(media, 2, "only the two accepted copies");
}

#[test]
fn import_in_three_steps_copies_outside_the_catalog() {
    use vm_storage::MediaLimits;
    let dir = common::tempdir();
    let src_dir = common::tempdir();
    let src = src_dir.path().join("data.iso");
    let mut content = vec![0u8; 48 * 1024];
    content[16 * 2048 + 1..16 * 2048 + 6].copy_from_slice(b"CD001");
    std::fs::write(&src, &content).unwrap();
    let limits = MediaLimits {
        max_bytes: 1 << 20,
        optical_signature: true,
    };
    let mut cat = Catalog::open(dir.path()).unwrap();
    let job = cat
        .begin_media_import(&src, MediaKind::Data, "built with oscdimg", limits)
        .unwrap();
    assert_eq!(job.size_bytes(), content.len() as u64);
    // While the job holds the source, nobody can write it.
    assert!(std::fs::OpenOptions::new().write(true).open(&src).is_err() || cfg!(not(windows)));
    // The catalog keeps serving while the copy runs on another thread.
    let copied = std::thread::spawn(move || job.copy()).join().unwrap();
    let other = common::new_vm(&mut cat, "meanwhile");
    assert!(cat.read(|tx| tx.vm(&other)).is_ok());
    let m = cat.finish_media_import(copied).unwrap();
    assert_eq!(m.sha256, sha_hex(&content));
    assert!(cat.open_intents().unwrap().is_empty());

    // A job never finished (service stopped mid-copy) is rolled back at the
    // next open: no row, no file, no temporary.
    let job = cat
        .begin_media_import(&src, MediaKind::Data, "x", limits)
        .unwrap();
    drop(job);
    drop(cat);
    let cat = Catalog::open(dir.path()).unwrap();
    assert!(cat.open_intents().unwrap().is_empty());
    assert_eq!(
        std::fs::read_dir(dir.path().join("tmp")).unwrap().count(),
        0
    );
    assert_eq!(
        std::fs::read_dir(dir.path().join("media")).unwrap().count(),
        1
    );
}

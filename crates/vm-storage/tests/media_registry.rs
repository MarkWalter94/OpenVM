//! Approved media registry (ADR-0008, ADR-0029; ROADMAP 7.4, "File" and
//! "Host" rows): the user's file used in place, SHA-256 read through the
//! handle at approval and before every use, no copy and nothing left behind;
//! managed copies of older catalogs still verified and deleted.

mod common;

use sha2::{Digest, Sha256};
use vm_storage::{
    Catalog, CatalogLayout, IntentKind, IntentSpec, IntentState, MediaId, MediaKind, MediaLocation,
    MediaSlot, MediaStatus, PathRejection, StorageError, TargetKind,
};

fn sha_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn entries(dir: &std::path::Path, sub: &str) -> usize {
    std::fs::read_dir(dir.join(sub)).map_or(0, Iterator::count)
}

/// Path the catalog records for a source (resolved by the system).
fn resolved(p: &std::path::Path) -> std::path::PathBuf {
    let f = std::fs::File::open(p).unwrap();
    vm_storage::canonical_source(&f, p).unwrap()
}

#[test]
fn approved_media_is_the_users_file_used_in_place_with_hash() {
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
    assert_eq!(m.location, MediaLocation::Referenced(resolved(&src)));
    // Nothing copied, nothing left behind, the source untouched.
    assert_eq!(entries(dir.path(), "media"), 0, "no managed copy");
    assert_eq!(entries(dir.path(), "tmp"), 0, "no temporary");
    assert!(cat.open_intents().unwrap().is_empty());
    assert_eq!(std::fs::read(&src).unwrap(), content);
    assert!(!std::fs::metadata(&src).unwrap().permissions().readonly());
    assert_eq!(cat.read(|tx| tx.media(&m.media_id)).unwrap(), m);
    let check = cat.verify_media(&m.media_id).unwrap();
    assert_eq!(check.record, m);
    assert!(!check.readonly_restored);

    // ISO replaced after approval: the use is refused, naming the file only.
    std::fs::write(&src, b"replaced content").unwrap();
    let err = cat.verify_media(&m.media_id).unwrap_err();
    let StorageError::IntegrityMismatch { path, .. } = &err else {
        panic!("{err:?}")
    };
    assert_eq!(path, "ubuntu.iso");
    // Content back: usable again.
    std::fs::write(&src, &content).unwrap();
    cat.verify_media(&m.media_id).unwrap();

    // Can be attached to a VM as installation media.
    let vm = common::new_vm(&mut cat, "vm");
    cat.with_tx(|tx| tx.attach_media(&vm, MediaSlot::Install, &m.media_id))
        .unwrap();
}

/// ADR-0029: selecting the same file again (after it changed) approves it
/// again in its own row, so the VMs that use it keep working; the same file
/// as the other kind is another medium.
#[test]
fn selecting_the_same_file_again_updates_its_row() {
    let dir = common::tempdir();
    let src_dir = common::tempdir();
    let src = src_dir.path().join("win.iso");
    std::fs::write(&src, b"CD001 first build").unwrap();
    let mut cat = Catalog::open(dir.path()).unwrap();
    let m = cat.approve_media(&src, MediaKind::Iso, "first").unwrap();
    std::fs::write(&src, b"CD001 second build, longer").unwrap();
    assert!(matches!(
        cat.verify_media(&m.media_id),
        Err(StorageError::IntegrityMismatch { .. })
    ));
    let again = cat.approve_media(&src, MediaKind::Iso, "second").unwrap();
    assert_eq!(again.media_id, m.media_id, "same row");
    assert_eq!(again.sha256, sha_hex(b"CD001 second build, longer"));
    assert_eq!(again.declared_origin, "second");
    assert_eq!(cat.read(|tx| tx.media_ids()).unwrap(), [m.media_id]);
    cat.verify_media(&m.media_id).unwrap();
    let data = cat.approve_media(&src, MediaKind::Data, "as data").unwrap();
    assert_ne!(data.media_id, m.media_id);
    assert_eq!(cat.read(|tx| tx.media_ids()).unwrap().len(), 2);
}

/// File moved or deleted: `missing` and not usable; back in place: usable
/// again after the hash.
#[test]
fn a_missing_file_marks_the_medium_missing_until_it_is_back() {
    let dir = common::tempdir();
    let src_dir = common::tempdir();
    let src = src_dir.path().join("data.iso");
    std::fs::write(&src, b"CD001 data").unwrap();
    let mut cat = Catalog::open(dir.path()).unwrap();
    let m = cat.approve_media(&src, MediaKind::Data, "fixture").unwrap();
    let moved = src_dir.path().join("elsewhere.iso");
    std::fs::rename(&src, &moved).unwrap();
    let err = cat.verify_media(&m.media_id).unwrap_err();
    assert!(
        matches!(&err, StorageError::NotFound { id, .. } if id == "data.iso"),
        "{err:?}"
    );
    assert_eq!(
        cat.read(|tx| tx.media(&m.media_id)).unwrap().status,
        MediaStatus::Missing
    );
    // Reconciliation does not look outside the catalog: still missing, the
    // row kept.
    drop(cat);
    let mut cat = Catalog::open(dir.path()).unwrap();
    assert_eq!(
        cat.read(|tx| tx.media(&m.media_id)).unwrap().status,
        MediaStatus::Missing
    );
    std::fs::rename(&moved, &src).unwrap();
    let check = cat.verify_media(&m.media_id).unwrap();
    assert_eq!(check.record.status, MediaStatus::Approved);
    assert_eq!(
        cat.read(|tx| tx.media(&m.media_id)).unwrap().status,
        MediaStatus::Approved
    );
}

#[test]
fn altered_managed_copy_is_detected() {
    let dir = common::tempdir();
    let mut cat = Catalog::open(dir.path()).unwrap();
    let m = common::managed_media(&mut cat, b"original data", MediaKind::Data, "fixture");
    let rel = m.location.managed().unwrap().clone();
    let copy = cat.layout().resolve(&rel).unwrap();
    assert!(std::fs::metadata(&copy).unwrap().permissions().readonly());
    cat.verify_media(&m.media_id).unwrap();
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

    // Copy removed: media `missing`, and it stays so.
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
    // No row, file or intent left behind by the refusals.
    assert!(cat.read(|tx| tx.media_ids()).unwrap().is_empty());
    assert_eq!(entries(dir.path(), "media"), 0);
    assert!(cat.open_intents().unwrap().is_empty());
}

/// A FIFO (or a link to one) is refused without blocking the open: with no
/// writer, a blocking open of a FIFO never returns.
#[cfg(unix)]
#[test]
fn source_that_is_a_fifo_is_refused_without_blocking() {
    let dir = common::tempdir();
    let src_dir = common::tempdir();
    let fifo = src_dir.path().join("pipe.iso");
    let made = std::process::Command::new("mkfifo")
        .arg(&fifo)
        .status()
        .is_ok_and(|s| s.success());
    if !made {
        eprintln!("mkfifo not available: test skipped");
        return;
    }
    let link = src_dir.path().join("link-to-pipe.iso");
    std::os::unix::fs::symlink(&fifo, &link).unwrap();
    let root = dir.path().to_path_buf();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut cat = Catalog::open(&root).unwrap();
        let fifo_err = cat.approve_media(&fifo, MediaKind::Iso, "x").unwrap_err();
        let link_err = cat.approve_media(&link, MediaKind::Iso, "x").unwrap_err();
        tx.send((fifo_err, link_err)).unwrap();
    });
    let (fifo_err, link_err) = rx
        .recv_timeout(std::time::Duration::from_secs(10))
        .expect("the open of a FIFO blocked");
    for err in [fifo_err, link_err] {
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
    }
}

/// The `import_media` intents of older catalogs still publish without
/// overwriting and roll back cleanly.
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

/// An `import_media` intent left open by an older version (service stopped
/// mid-copy) is still rolled back at the next open: no row, no file.
#[test]
fn an_unfinished_import_of_an_older_version_still_reconciles() {
    let dir = common::tempdir();
    let mut cat = Catalog::open(dir.path()).unwrap();
    let id = MediaId::new_v7();
    let i = cat
        .begin_intent(IntentSpec {
            kind: IntentKind::ImportMedia,
            vm_id: None,
            operation_id: None,
            target_kind: TargetKind::Media,
            target_id: Some(id.to_string()),
            final_path: Some(CatalogLayout::media_file(&id, "iso").unwrap()),
            read_only: true,
        })
        .unwrap();
    common::write_bytes(&i.temp_path, b"half a copy").unwrap();
    drop(cat);
    let cat = Catalog::open(dir.path()).unwrap();
    assert!(cat.open_intents().unwrap().is_empty());
    assert_eq!(entries(dir.path(), "tmp"), 0);
    assert_eq!(entries(dir.path(), "media"), 0);
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
    // Wrong signature and file too short: refused, nothing recorded.
    for p in [
        image("zip.iso", b"PK\x03\x04\x00", 40 * 1024),
        image("short.iso", b"", 1024),
    ] {
        match cat.approve_media_checked(&p, MediaKind::Data, "x", strict) {
            Err(StorageError::Unsupported(m)) => assert_eq!(m, NOT_OPTICAL_IMAGE),
            other => panic!("{other:?}"),
        }
    }
    // Over the size limit: refused before reading.
    let big = image("big.iso", b"CD001", 65 * 1024);
    assert!(matches!(
        cat.approve_media_checked(&big, MediaKind::Iso, "x", strict),
        Err(StorageError::ResourceLimit(_))
    ));
    assert!(cat.open_intents().unwrap().is_empty());
    assert_eq!(entries(dir.path(), "tmp"), 0);
    assert_eq!(entries(dir.path(), "media"), 0, "nothing copied");
    assert_eq!(cat.read(|tx| tx.media_ids()).unwrap().len(), 2);
}

#[test]
fn import_in_three_steps_hashes_outside_the_catalog() {
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
    let r = cat
        .reserve_media_import(&src, MediaKind::Data, "built with oscdimg", limits)
        .unwrap();
    // The reservation records nothing.
    assert!(cat.open_intents().unwrap().is_empty());
    assert!(cat.read(|tx| tx.media_ids()).unwrap().is_empty());
    // The catalog keeps serving while the hash runs on another thread.
    let hashed = std::thread::spawn(move || r.open_and_hash(vm_storage::canonical_source))
        .join()
        .unwrap();
    // Until the row is written the source is held: nobody can write it.
    #[cfg(windows)]
    assert!(std::fs::OpenOptions::new().write(true).open(&src).is_err());
    let other = common::new_vm(&mut cat, "meanwhile");
    assert!(cat.read(|tx| tx.vm(&other)).is_ok());
    let m = cat.finish_media_import(hashed).unwrap();
    assert_eq!(m.sha256, sha_hex(&content));
    assert_eq!(m.location, MediaLocation::Referenced(resolved(&src)));
    // Released after the row is written.
    drop(std::fs::OpenOptions::new().write(true).open(&src).unwrap());

    // An import abandoned at any step (service stopped mid-hash) leaves
    // nothing to reconcile: no intent, no row, no file.
    let r = cat
        .reserve_media_import(&src, MediaKind::Iso, "x", limits)
        .unwrap();
    let hashed = r.open_and_hash(vm_storage::canonical_source);
    drop(hashed);
    drop(cat);
    let cat = Catalog::open(dir.path()).unwrap();
    assert!(cat.open_report().unwrap().recovery.intents.is_empty());
    assert!(cat.open_intents().unwrap().is_empty());
    assert_eq!(entries(dir.path(), "tmp"), 0);
    assert_eq!(entries(dir.path(), "media"), 0);
}

#[test]
fn reserved_import_opens_the_source_only_on_the_worker() {
    use vm_storage::MediaLimits;
    let dir = common::tempdir();
    let src_dir = common::tempdir();
    let src = src_dir.path().join("late.iso");
    let mut content = vec![0u8; 40 * 1024];
    content[16 * 2048 + 1..16 * 2048 + 6].copy_from_slice(b"CD001");
    let limits = MediaLimits {
        max_bytes: 1 << 20,
        optical_signature: true,
    };
    let mut cat = Catalog::open(dir.path()).unwrap();
    // The reservation never touches the source: it may not exist yet.
    let r = cat
        .reserve_media_import(&src, MediaKind::Data, "x", limits)
        .unwrap();
    std::fs::write(&src, &content).unwrap();
    let mut checked = false;
    let hashed = r.open_and_hash(|f, p| {
        checked = f.metadata().unwrap().len() == content.len() as u64 && p == src.as_path();
        vm_storage::canonical_source(f, p)
    });
    assert!(checked);
    let m = cat.finish_media_import(hashed).unwrap();
    assert_eq!(m.sha256, sha_hex(&content));

    // Rejected by the caller's check: nothing recorded.
    let r = cat
        .reserve_media_import(&src, MediaKind::Iso, "x", limits)
        .unwrap();
    let hashed = r.open_and_hash(|_, _| Err(StorageError::Unsupported("remote volume")));
    assert!(matches!(
        cat.finish_media_import(hashed),
        Err(StorageError::Unsupported("remote volume"))
    ));
    // A relative path to record is refused.
    let r = cat
        .reserve_media_import(&src, MediaKind::Iso, "x", limits)
        .unwrap();
    let hashed = r.open_and_hash(|_, _| Ok("late.iso".into()));
    assert!(cat.finish_media_import(hashed).is_err());

    // Missing source: the error comes from the worker step.
    let r = cat
        .reserve_media_import(
            &src_dir.path().join("missing.iso"),
            MediaKind::Iso,
            "x",
            limits,
        )
        .unwrap();
    let hashed = r.open_and_hash(vm_storage::canonical_source);
    assert!(cat.finish_media_import(hashed).is_err());

    // Cancelled after the hash: nothing recorded.
    let r = cat
        .reserve_media_import(&src, MediaKind::Iso, "x", limits)
        .unwrap();
    let hashed = r.open_and_hash(vm_storage::canonical_source);
    cat.abort_media_import(hashed).unwrap();
    assert_eq!(cat.read(|tx| tx.media_ids()).unwrap(), [m.media_id]);
    assert!(cat.open_intents().unwrap().is_empty());
    assert_eq!(entries(dir.path(), "tmp"), 0);
    assert_eq!(entries(dir.path(), "media"), 0);
}

#[test]
fn detected_guest_is_recorded_with_the_row_and_can_be_updated() {
    use lightvm_proto::{DetectedGuest, GuestArch, GuestOs, Label};
    use std::io::Read as _;
    let dir = common::tempdir();
    let src_dir = common::tempdir();
    let src = src_dir.path().join("win7.iso");
    let content = b"CD001 fake windows medium".repeat(100);
    std::fs::write(&src, &content).unwrap();
    let mut cat = Catalog::open(dir.path()).unwrap();
    let r = cat
        .reserve_media_import(&src, MediaKind::Iso, "x", vm_storage::MediaLimits::NONE)
        .unwrap();
    let mut hashed = r.open_and_hash(vm_storage::canonical_source);
    // The hashed file can be examined through the held handle.
    let mut f = hashed.source_file().expect("hash succeeded");
    std::io::Seek::seek(&mut f, std::io::SeekFrom::Start(0)).unwrap();
    let mut read = Vec::new();
    f.read_to_end(&mut read).unwrap();
    assert_eq!(read, content);
    let guest = DetectedGuest {
        os: GuestOs::Windows7,
        arch: Some(GuestArch::X86_64),
        build: Some(7601),
        edition: Some(Label::parse("Windows 7 Professional").unwrap()),
    };
    hashed.set_detected_guest(Some(guest.clone()));
    let m = cat.finish_media_import(hashed).unwrap();
    assert_eq!(m.detected_guest.as_ref(), Some(&guest));
    assert!(m.guest_examined);
    assert_eq!(cat.read(|tx| tx.media(&m.media_id)).unwrap(), m);

    // Not examined: stored as such.
    let src2 = src_dir.path().join("data.iso");
    std::fs::write(&src2, b"data").unwrap();
    let d = cat.approve_media(&src2, MediaKind::Data, "x").unwrap();
    assert!(!d.guest_examined && d.detected_guest.is_none());
    // Examined later with no result, then with one.
    cat.with_tx(|tx| tx.set_media_detected_guest(&d.media_id, None))
        .unwrap();
    let r = cat.read(|tx| tx.media(&d.media_id)).unwrap();
    assert!(r.guest_examined && r.detected_guest.is_none());
    cat.with_tx(|tx| tx.set_media_detected_guest(&d.media_id, Some(&guest)))
        .unwrap();
    let r = cat.read(|tx| tx.media(&d.media_id)).unwrap();
    assert_eq!(r.detected_guest, Some(guest));
    // Unknown media.
    assert!(matches!(
        cat.with_tx(|tx| tx.set_media_detected_guest(&MediaId::new_v7(), None)),
        Err(StorageError::NotFound { .. })
    ));
    // A value this version cannot read is a lost hint, not a broken registry.
    let conn = vm_storage::migrate::open_connection(&dir.path().join(vm_storage::DB_FILE)).unwrap();
    conn.execute(
        "UPDATE media SET detected_guest = '{\"os\":\"windows_99\"}' WHERE media_id = ?1",
        rusqlite::params![d.media_id.to_string()],
    )
    .unwrap();
    let r = cat.read(|tx| tx.media(&d.media_id)).unwrap();
    assert!(r.detected_guest.is_none() && r.guest_examined);
}

/// The guard of a start on a file used in place: every component checked
/// against links (a junction in a directory is refused), the file pinned
/// while the guard lives (Windows share mode), a missing file `NotFound`.
#[test]
fn guard_of_a_file_used_in_place() {
    let src_dir = common::tempdir();
    let real = src_dir.path().join("real");
    std::fs::create_dir_all(&real).unwrap();
    let file = real.join("pinned.iso");
    std::fs::write(&file, b"CD001 pinned").unwrap();
    let path = resolved(&file);
    let g = vm_storage::MediaGuard::open_external(&path).unwrap();
    assert_eq!(g.path(), path.as_path());
    assert_eq!(g.size_bytes(), 12);
    #[cfg(windows)]
    {
        assert!(std::fs::OpenOptions::new().write(true).open(&path).is_err());
        assert!(std::fs::rename(&path, real.join("other.iso")).is_err());
        assert!(std::fs::remove_file(&path).is_err());
        assert!(std::fs::rename(&real, src_dir.path().join("real2")).is_err());
    }
    let (hex, n) = g.sha256_with(&|| false, &|_| {}).unwrap();
    assert_eq!((hex, n), (sha_hex(b"CD001 pinned"), 12));
    drop(g);
    // A directory reached through a junction (or symbolic link) is refused.
    let link = src_dir.path().join("via-link");
    #[cfg(unix)]
    let made = std::os::unix::fs::symlink(&real, &link).is_ok();
    #[cfg(windows)]
    let made = common::make_junction(&link, &real);
    if made {
        let err = vm_storage::MediaGuard::open_external(&link.join("pinned.iso")).unwrap_err();
        assert!(
            matches!(
                &err,
                StorageError::InvalidPath {
                    reason: PathRejection::Link,
                    ..
                }
            ),
            "{err:?}"
        );
        assert!(
            !err.to_string().contains("via-link"),
            "file name only: {err}"
        );
    } else {
        eprintln!("junction not available: part skipped");
    }
    // Relative path, missing file.
    assert!(vm_storage::MediaGuard::open_external(std::path::Path::new("pinned.iso")).is_err());
    std::fs::remove_file(&path).unwrap();
    match vm_storage::MediaGuard::open_external(&path) {
        Err(StorageError::Io { source, .. }) => {
            assert_eq!(source.kind(), std::io::ErrorKind::NotFound);
        }
        other => panic!("{other:?}"),
    }
}

/// Medium with its managed copy (older catalogs), for the deletion tests.
fn approved(cat: &mut Catalog, name: &str) -> vm_storage::MediaRecord {
    common::managed_media(
        cat,
        format!("CD001 fake medium {name}").repeat(200).as_bytes(),
        MediaKind::Iso,
        name,
    )
}

fn managed_copy(cat: &Catalog, m: &vm_storage::MediaRecord) -> std::path::PathBuf {
    cat.layout()
        .resolve(m.location.managed().expect("managed copy"))
        .unwrap()
}

/// ADR-0029: deleting a medium used in place removes its row only; the
/// user's file is never touched.
#[test]
fn deleting_a_medium_used_in_place_keeps_the_users_file() {
    let dir = common::tempdir();
    let src_dir = common::tempdir();
    let src = src_dir.path().join("keep-me.iso");
    let content = b"CD001 the user's own file".repeat(300);
    std::fs::write(&src, &content).unwrap();
    let mut cat = Catalog::open(dir.path()).unwrap();
    let m = cat.approve_media(&src, MediaKind::Iso, "mine").unwrap();
    let vm = common::new_vm(&mut cat, "user");
    cat.with_tx(|tx| tx.attach_media(&vm, MediaSlot::Install, &m.media_id))
        .unwrap();
    assert!(matches!(
        cat.delete_media(&m.media_id),
        Err(StorageError::StateConflict(_))
    ));
    cat.with_tx(|tx| tx.detach_media(&vm, MediaSlot::Install))
        .unwrap();
    cat.delete_media(&m.media_id).unwrap();
    assert!(matches!(
        cat.read(|tx| tx.media(&m.media_id)),
        Err(StorageError::NotFound { .. })
    ));
    assert_eq!(std::fs::read(&src).unwrap(), content, "file kept as it was");
    assert!(cat.open_intents().unwrap().is_empty());
    assert_eq!(entries(dir.path(), "tmp"), 0);
    // A medium whose file is already gone loses its row the same way.
    let gone = cat.approve_media(&src, MediaKind::Data, "gone").unwrap();
    let elsewhere = src_dir.path().join("moved.iso");
    std::fs::rename(&src, &elsewhere).unwrap();
    assert!(cat.verify_media(&gone.media_id).is_err());
    cat.delete_media(&gone.media_id).unwrap();
    assert!(cat.read(|tx| tx.media_ids()).unwrap().is_empty());
    assert_eq!(std::fs::read(&elsewhere).unwrap(), content);
}

#[test]
fn deleting_a_medium_removes_the_readonly_copy_and_the_row() {
    let dir = common::tempdir();
    let mut cat = Catalog::open(dir.path()).unwrap();
    let m = approved(&mut cat, "a.iso");
    let keep = approved(&mut cat, "b.iso");
    let copy = managed_copy(&cat, &m);
    assert!(std::fs::metadata(&copy).unwrap().permissions().readonly());

    cat.delete_media(&m.media_id).unwrap();
    assert!(!copy.exists());
    assert!(matches!(
        cat.read(|tx| tx.media(&m.media_id)),
        Err(StorageError::NotFound { .. })
    ));
    assert!(cat.open_intents().unwrap().is_empty());
    assert_eq!(entries(dir.path(), "tmp"), 0, "no leftover temporary");
    // The other medium is untouched.
    assert_eq!(cat.read(|tx| tx.media_ids()).unwrap(), [keep.media_id]);
    cat.verify_media(&keep.media_id).unwrap();
    // A second deletion finds nothing.
    assert!(matches!(
        cat.delete_media(&m.media_id),
        Err(StorageError::NotFound { .. })
    ));
}

#[test]
fn an_attached_medium_is_not_deleted_and_the_refusal_names_the_vms() {
    let dir = common::tempdir();
    let mut cat = Catalog::open(dir.path()).unwrap();
    let m = approved(&mut cat, "a.iso");
    let one = common::new_vm(&mut cat, "web-test");
    let two = common::new_vm(&mut cat, "api-test");
    cat.with_tx(|tx| {
        tx.attach_media(&one, MediaSlot::Install, &m.media_id)?;
        tx.attach_media(&two, MediaSlot::Data, &m.media_id)
    })
    .unwrap();
    let err = cat.delete_media(&m.media_id).unwrap_err();
    let StorageError::StateConflict(msg) = &err else {
        panic!("{err:?}")
    };
    assert_eq!(
        msg,
        "the medium is used by 2 VMs: \u{201c}api-test\u{201d}, \u{201c}web-test\u{201d}"
    );
    let copy = managed_copy(&cat, &m);
    assert!(std::fs::metadata(&copy).unwrap().permissions().readonly());
    assert!(cat.open_intents().unwrap().is_empty());
    cat.verify_media(&m.media_id).unwrap();

    cat.with_tx(|tx| tx.detach_media(&two, MediaSlot::Data))
        .unwrap();
    let err = cat.delete_media(&m.media_id).unwrap_err();
    assert!(
        err.to_string()
            .contains("used by 1 VM: \u{201c}web-test\u{201d}"),
        "{err}"
    );
    cat.with_tx(|tx| tx.detach_media(&one, MediaSlot::Install))
        .unwrap();
    cat.delete_media(&m.media_id).unwrap();
    assert!(!copy.exists());
}

/// The guard of a run opens the copy with `FILE_SHARE_READ` only: the
/// deletion is refused and nothing changes (Windows share modes).
#[cfg(windows)]
#[test]
fn a_medium_held_by_a_run_is_not_deleted() {
    let dir = common::tempdir();
    let mut cat = Catalog::open(dir.path()).unwrap();
    let m = approved(&mut cat, "a.iso");
    let copy = managed_copy(&cat, &m);
    let guard = vm_storage::MediaGuard::open(dir.path(), m.location.managed().unwrap()).unwrap();
    let err = cat.delete_media(&m.media_id).unwrap_err();
    assert!(
        matches!(&err, StorageError::VmBusy(msg) if msg.contains("held open by a running VM")),
        "{err:?}"
    );
    // Nothing changed: copy in place and read-only, row present, intent
    // aborted.
    assert!(std::fs::metadata(&copy).unwrap().permissions().readonly());
    assert!(cat.read(|tx| tx.media(&m.media_id)).is_ok());
    assert!(cat.open_intents().unwrap().is_empty());
    assert_eq!(entries(dir.path(), "tmp"), 0);
    drop(guard);
    cat.delete_media(&m.media_id).unwrap();
    assert!(!copy.exists());
}

#[test]
fn a_medium_whose_copy_is_missing_loses_only_its_row() {
    let dir = common::tempdir();
    let mut cat = Catalog::open(dir.path()).unwrap();
    let m = approved(&mut cat, "a.iso");
    let copy = managed_copy(&cat, &m);
    #[allow(clippy::permissions_set_readonly_false)]
    {
        let mut p = std::fs::metadata(&copy).unwrap().permissions();
        p.set_readonly(false);
        std::fs::set_permissions(&copy, p).unwrap();
    }
    std::fs::remove_file(&copy).unwrap();
    assert!(cat.verify_media(&m.media_id).is_err());
    assert_eq!(
        cat.read(|tx| tx.media(&m.media_id)).unwrap().status,
        MediaStatus::Missing
    );
    cat.delete_media(&m.media_id).unwrap();
    assert!(cat.read(|tx| tx.media_ids()).unwrap().is_empty());
}

/// Deletion stopped at `point` as if the process had died; `meanwhile` runs
/// on the catalog before it is dropped and reopened (reconciliation).
fn crash_during_deletion(
    point: vm_storage::FaultPoint,
    meanwhile: impl FnOnce(&mut Catalog, &vm_storage::MediaRecord),
) -> (tempfile::TempDir, Catalog, vm_storage::MediaRecord) {
    let dir = common::tempdir();
    let mut cat = Catalog::open(dir.path()).unwrap();
    let m = approved(&mut cat, "a.iso");
    cat.inject_fault(point, vm_storage::FaultAction::Crash);
    let err = cat.delete_media(&m.media_id).unwrap_err();
    assert!(
        matches!(err, StorageError::InjectedCrash(p) if p == point),
        "{err:?}"
    );
    meanwhile(&mut cat, &m);
    drop(cat);
    let cat = Catalog::open(dir.path()).unwrap();
    (dir, cat, m)
}

#[test]
fn a_deletion_interrupted_after_the_move_is_completed_at_the_next_start() {
    let (dir, mut cat, m) =
        crash_during_deletion(vm_storage::FaultPoint::DeleteBeforeCommit, |cat, m| {
            // Interrupted with the copy moved into `tmp/` and the row still
            // there.
            assert!(!managed_copy(cat, m).exists());
            assert_eq!(cat.open_intents().unwrap().len(), 1);
            assert!(cat.read(|tx| tx.media(&m.media_id)).is_ok());
        });
    let report = cat.open_report().unwrap().clone();
    assert!(
        report
            .recovery
            .intents
            .iter()
            .any(|i| i.kind == IntentKind::DeleteFile
                && i.outcome == vm_storage::RecoveryOutcome::RolledForward),
        "{report:?}"
    );
    assert!(matches!(
        cat.read(|tx| tx.media(&m.media_id)),
        Err(StorageError::NotFound { .. })
    ));
    assert!(!managed_copy(&cat, &m).exists());
    assert!(cat.open_intents().unwrap().is_empty());
    assert_eq!(entries(dir.path(), "tmp"), 0, "read-only temporary removed");
}

#[test]
fn a_deletion_interrupted_when_a_vm_was_attached_meanwhile_restores_the_copy() {
    let (dir, mut cat, m) =
        crash_during_deletion(vm_storage::FaultPoint::DeleteBeforeCommit, |cat, m| {
            let vm = common::new_vm(cat, "late");
            cat.with_tx(|tx| tx.attach_media(&vm, MediaSlot::Install, &m.media_id))
                .unwrap();
        });
    let copy = managed_copy(&cat, &m);
    assert!(copy.exists(), "copy moved back in place");
    assert!(std::fs::metadata(&copy).unwrap().permissions().readonly());
    assert_eq!(
        cat.read(|tx| tx.media(&m.media_id)).unwrap().status,
        MediaStatus::Approved
    );
    cat.verify_media(&m.media_id).unwrap();
    assert!(cat.open_intents().unwrap().is_empty());
    assert_eq!(entries(dir.path(), "tmp"), 0);
}

#[test]
fn a_deletion_interrupted_after_the_commit_leaves_no_temporary() {
    let (dir, mut cat, m) = crash_during_deletion(vm_storage::FaultPoint::AfterCommit, |_, _| {});
    assert!(matches!(
        cat.read(|tx| tx.media(&m.media_id)),
        Err(StorageError::NotFound { .. })
    ));
    assert!(cat.open_intents().unwrap().is_empty());
    assert_eq!(entries(dir.path(), "tmp"), 0, "read-only temporary removed");
}

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
    assert_eq!(cat.open_intents().unwrap().len(), 1);
    std::fs::write(&src, &content).unwrap();
    let mut checked = false;
    let copied = r.open_and_copy(|f| {
        checked = f.metadata().unwrap().len() == content.len() as u64;
        Ok(())
    });
    assert!(checked);
    let m = cat.finish_media_import(copied).unwrap();
    assert_eq!(m.sha256, sha_hex(&content));
    assert!(cat.open_intents().unwrap().is_empty());

    // Rejected by the caller's check: nothing copied, intent aborted.
    let r = cat
        .reserve_media_import(&src, MediaKind::Data, "x", limits)
        .unwrap();
    let copied = r.open_and_copy(|_| Err(StorageError::Unsupported("remote volume")));
    assert!(matches!(
        cat.finish_media_import(copied),
        Err(StorageError::Unsupported("remote volume"))
    ));
    assert!(cat.open_intents().unwrap().is_empty());

    // Missing source: the error comes from the worker step.
    let r = cat
        .reserve_media_import(
            &src_dir.path().join("missing.iso"),
            MediaKind::Data,
            "x",
            limits,
        )
        .unwrap();
    let copied = r.open_and_copy(|_| Ok(()));
    assert!(cat.finish_media_import(copied).is_err());

    // Cancelled after the copy: aborted, temporary removed, nothing published.
    let r = cat
        .reserve_media_import(&src, MediaKind::Data, "x", limits)
        .unwrap();
    let copied = r.open_and_copy(|_| Ok(()));
    cat.abort_media_import(copied).unwrap();
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

#[test]
fn detected_guest_is_recorded_with_the_copy_and_can_be_updated() {
    use lightvm_proto::{DetectedGuest, GuestArch, GuestOs, Label};
    let dir = common::tempdir();
    let src_dir = common::tempdir();
    let src = src_dir.path().join("win7.iso");
    let content = b"CD001 fake windows medium".repeat(100);
    std::fs::write(&src, &content).unwrap();
    let mut cat = Catalog::open(dir.path()).unwrap();
    let job = cat
        .begin_media_import(&src, MediaKind::Iso, "x", vm_storage::MediaLimits::NONE)
        .unwrap();
    let mut copied = job.copy();
    // The copied bytes can be examined before publication.
    let file = copied.copied_file().expect("copy succeeded").to_path_buf();
    assert_eq!(std::fs::read(&file).unwrap(), content);
    let guest = DetectedGuest {
        os: GuestOs::Windows7,
        arch: Some(GuestArch::X86_64),
        build: Some(7601),
        edition: Some(Label::parse("Windows 7 Professional").unwrap()),
    };
    copied.set_detected_guest(Some(guest.clone()));
    let m = cat.finish_media_import(copied).unwrap();
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

/// Medium with its managed copy, for the deletion tests.
fn approved(cat: &mut Catalog, src_dir: &std::path::Path, name: &str) -> vm_storage::MediaRecord {
    let src = src_dir.join(name);
    std::fs::write(&src, format!("CD001 fake medium {name}").repeat(200)).unwrap();
    cat.approve_media(&src, MediaKind::Iso, name).unwrap()
}

fn tmp_entries(dir: &std::path::Path) -> usize {
    std::fs::read_dir(dir.join("tmp")).map_or(0, Iterator::count)
}

#[test]
fn deleting_a_medium_removes_the_readonly_copy_and_the_row() {
    let dir = common::tempdir();
    let src_dir = common::tempdir();
    let mut cat = Catalog::open(dir.path()).unwrap();
    let m = approved(&mut cat, src_dir.path(), "a.iso");
    let keep = approved(&mut cat, src_dir.path(), "b.iso");
    let copy = cat.layout().resolve(&m.managed_path).unwrap();
    assert!(std::fs::metadata(&copy).unwrap().permissions().readonly());

    cat.delete_media(&m.media_id).unwrap();
    assert!(!copy.exists());
    assert!(matches!(
        cat.read(|tx| tx.media(&m.media_id)),
        Err(StorageError::NotFound { .. })
    ));
    assert!(cat.open_intents().unwrap().is_empty());
    assert_eq!(tmp_entries(dir.path()), 0, "no leftover temporary");
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
    let src_dir = common::tempdir();
    let mut cat = Catalog::open(dir.path()).unwrap();
    let m = approved(&mut cat, src_dir.path(), "a.iso");
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
    let copy = cat.layout().resolve(&m.managed_path).unwrap();
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
    let src_dir = common::tempdir();
    let mut cat = Catalog::open(dir.path()).unwrap();
    let m = approved(&mut cat, src_dir.path(), "a.iso");
    let copy = cat.layout().resolve(&m.managed_path).unwrap();
    let guard = vm_storage::MediaGuard::open(dir.path(), &m.managed_path).unwrap();
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
    assert_eq!(tmp_entries(dir.path()), 0);
    drop(guard);
    cat.delete_media(&m.media_id).unwrap();
    assert!(!copy.exists());
}

#[test]
fn a_medium_whose_copy_is_missing_loses_only_its_row() {
    let dir = common::tempdir();
    let src_dir = common::tempdir();
    let mut cat = Catalog::open(dir.path()).unwrap();
    let m = approved(&mut cat, src_dir.path(), "a.iso");
    let copy = cat.layout().resolve(&m.managed_path).unwrap();
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
    let src_dir = common::tempdir();
    let mut cat = Catalog::open(dir.path()).unwrap();
    let m = approved(&mut cat, src_dir.path(), "a.iso");
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
            let copy = cat.layout().resolve(&m.managed_path).unwrap();
            assert!(!copy.exists());
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
    assert!(!cat.layout().resolve(&m.managed_path).unwrap().exists());
    assert!(cat.open_intents().unwrap().is_empty());
    assert_eq!(tmp_entries(dir.path()), 0, "read-only temporary removed");
}

#[test]
fn a_deletion_interrupted_when_a_vm_was_attached_meanwhile_restores_the_copy() {
    let (dir, mut cat, m) =
        crash_during_deletion(vm_storage::FaultPoint::DeleteBeforeCommit, |cat, m| {
            let vm = common::new_vm(cat, "late");
            cat.with_tx(|tx| tx.attach_media(&vm, MediaSlot::Install, &m.media_id))
                .unwrap();
        });
    let copy = cat.layout().resolve(&m.managed_path).unwrap();
    assert!(copy.exists(), "copy moved back in place");
    assert!(std::fs::metadata(&copy).unwrap().permissions().readonly());
    assert_eq!(
        cat.read(|tx| tx.media(&m.media_id)).unwrap().status,
        MediaStatus::Approved
    );
    cat.verify_media(&m.media_id).unwrap();
    assert!(cat.open_intents().unwrap().is_empty());
    assert_eq!(tmp_entries(dir.path()), 0);
}

#[test]
fn a_deletion_interrupted_after_the_commit_leaves_no_temporary() {
    let (dir, mut cat, m) = crash_during_deletion(vm_storage::FaultPoint::AfterCommit, |_, _| {});
    assert!(matches!(
        cat.read(|tx| tx.media(&m.media_id)),
        Err(StorageError::NotFound { .. })
    ));
    assert!(cat.open_intents().unwrap().is_empty());
    assert_eq!(tmp_entries(dir.path()), 0, "read-only temporary removed");
}

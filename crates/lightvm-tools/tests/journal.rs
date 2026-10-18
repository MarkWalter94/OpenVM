use lightvm_tools::{journal::Journal, policy::Secret, protocol::RelativePath};
use std::path::PathBuf;
struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "lightvm-journal-{}",
            Secret::generate().unwrap().export()
        ));
        std::fs::create_dir(&path).unwrap();
        Self(std::fs::canonicalize(path).unwrap())
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
#[test]
fn recovery_removes_only_journalled_staging_and_preserves_final_files() {
    let journal_dir = Temp::new();
    let share = Temp::new();
    let journal = Journal::open(&journal_dir.0).unwrap();
    let id = Secret::generate().unwrap().export();
    let path = RelativePath::new(vec!["final.bin".into()]).unwrap();
    // A process dies after preparing the journal and writing a partial file.
    let cleanup = journal.prepare(&share.0, &path, &id).unwrap();
    let stage = share.0.join(format!(".lightvm-transfer-{id}"));
    std::fs::write(&stage, b"partial").unwrap();
    std::fs::write(share.0.join("final.bin"), b"original").unwrap();
    drop(cleanup);
    drop(journal);
    let journal = Journal::open(&journal_dir.0).unwrap();
    assert_eq!(journal.recover().unwrap(), 1);
    assert!(!stage.exists());
    assert_eq!(
        std::fs::read(share.0.join("final.bin")).unwrap(),
        b"original"
    );
    assert_eq!(journal.recover().unwrap(), 0);
}
#[test]
fn recovery_after_publication_never_removes_destination() {
    let journal_dir = Temp::new();
    let share = Temp::new();
    let journal = Journal::open(&journal_dir.0).unwrap();
    let id = Secret::generate().unwrap().export();
    let path = RelativePath::new(vec!["published.bin".into()]).unwrap();
    let _cleanup = journal.prepare(&share.0, &path, &id).unwrap();
    std::fs::write(share.0.join("published.bin"), b"committed").unwrap();
    assert_eq!(journal.recover().unwrap(), 1);
    assert_eq!(
        std::fs::read(share.0.join("published.bin")).unwrap(),
        b"committed"
    );
}
#[cfg(unix)]
#[test]
fn recovery_never_follows_a_replaced_staging_symlink() {
    let journal_dir = Temp::new();
    let share = Temp::new();
    let journal = Journal::open(&journal_dir.0).unwrap();
    let id = Secret::generate().unwrap().export();
    let path = RelativePath::new(vec!["final".into()]).unwrap();
    let _cleanup = journal.prepare(&share.0, &path, &id).unwrap();
    let external = share.0.join("untouched");
    std::fs::write(&external, b"keep").unwrap();
    let link = share.0.join(format!(".lightvm-transfer-{id}"));
    std::os::unix::fs::symlink(&external, &link).unwrap();
    // The record is dropped; neither the link nor its target is touched.
    assert_eq!(journal.recover().unwrap(), 1);
    assert!(link.symlink_metadata().unwrap().file_type().is_symlink());
    assert_eq!(std::fs::read(external).unwrap(), b"keep");
    assert_eq!(journal.recover().unwrap(), 0);
}
#[test]
fn unexpected_staging_entry_drops_the_record_without_blocking_recovery() {
    let journal_dir = Temp::new();
    let share = Temp::new();
    let journal = Journal::open(&journal_dir.0).unwrap();
    let id = Secret::generate().unwrap().export();
    let path = RelativePath::new(vec!["final".into()]).unwrap();
    let _cleanup = journal.prepare(&share.0, &path, &id).unwrap();
    // The staging name was replaced by a non-empty directory.
    let stage = share.0.join(format!(".lightvm-transfer-{id}"));
    std::fs::create_dir(&stage).unwrap();
    std::fs::write(stage.join("keep"), b"keep").unwrap();
    // A second record whose parent directory no longer exists.
    let moved = RelativePath::new(vec!["gone".into(), "final".into()]).unwrap();
    let other = Secret::generate().unwrap().export();
    let _moved = journal.prepare(&share.0, &moved, &other).unwrap();
    assert_eq!(journal.recover().unwrap(), 2);
    assert_eq!(std::fs::read(stage.join("keep")).unwrap(), b"keep");
    assert_eq!(std::fs::read_dir(&journal_dir.0).unwrap().count(), 0);
    assert_eq!(journal.recover().unwrap(), 0);
}

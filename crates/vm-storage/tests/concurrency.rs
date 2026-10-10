//! Single catalog instance under concurrency: two or more `Catalog`s on the
//! same directory (concurrent threads, different spellings of the path).
//! The basic case (second open, sequential and from another thread) is in
//! `paths_lock.rs`.

mod common;

use std::sync::{Arc, Barrier};

use vm_storage::{Catalog, StorageError};

#[test]
fn concurrent_opens_admit_exactly_one_instance() {
    let dir = common::tempdir();
    // Catalog already created: the race concerns only the lock, not migrations.
    drop(Catalog::open(dir.path()).unwrap());
    const N: usize = 8;
    let barrier = Arc::new(Barrier::new(N));
    let (tx, rx) = std::sync::mpsc::channel();
    let release = Arc::new(Barrier::new(N + 1));
    let handles: Vec<_> = (0..N)
        .map(|_| {
            let p = dir.path().to_path_buf();
            let barrier = Arc::clone(&barrier);
            let release = Arc::clone(&release);
            let tx = tx.clone();
            std::thread::spawn(move || {
                barrier.wait();
                let res = Catalog::open(&p);
                tx.send(match &res {
                    Ok(_) => Ok(()),
                    Err(StorageError::CatalogLocked(_)) => Err(true),
                    Err(e) => panic!("unexpected error: {e:?}"),
                })
                .unwrap();
                // Everyone holds the result until the test has counted.
                release.wait();
                drop(res);
            })
        })
        .collect();
    let results: Vec<_> = (0..N).map(|_| rx.recv().unwrap()).collect();
    release.wait();
    for h in handles {
        h.join().unwrap();
    }
    let ok = results.iter().filter(|r| r.is_ok()).count();
    assert_eq!(ok, 1, "exactly one instance: {results:?}");
    assert_eq!(results.iter().filter(|r| r.is_err()).count(), N - 1);
    // Once all are released, the catalog reopens and has no open intents.
    let cat = Catalog::open(dir.path()).unwrap();
    assert!(cat.open_intents().unwrap().is_empty());
}

#[test]
fn other_spellings_of_the_same_directory_are_refused() {
    let dir = common::tempdir();
    let first = Catalog::open(dir.path()).unwrap();
    // `.` and `sub/..` components in the path.
    let dotted = dir.path().join(".");
    let sub = dir.path().join("vms");
    let parent_via_sub = sub.join("..");
    for p in [dotted, parent_via_sub] {
        match Catalog::open(&p) {
            Err(StorageError::CatalogLocked(_)) => {}
            other => panic!("{} accepted: {other:?}", p.display()),
        }
    }
    // On Windows, a spelling with different letter case is also the same directory.
    #[cfg(windows)]
    {
        let upper = std::path::PathBuf::from(dir.path().to_string_lossy().to_uppercase());
        match Catalog::open(&upper) {
            Err(StorageError::CatalogLocked(_)) => {}
            other => panic!("{} accepted: {other:?}", upper.display()),
        }
    }
    drop(first);
}

#[test]
fn distinct_catalogs_coexist() {
    let a = common::tempdir();
    let b = common::tempdir();
    let mut ca = Catalog::open(a.path()).unwrap();
    let mut cb = Catalog::open(b.path()).unwrap();
    let va = common::new_vm(&mut ca, "same-name");
    let vb = common::new_vm(&mut cb, "same-name");
    assert_ne!(va, vb);
    assert_eq!(ca.read(|tx| tx.vm_ids()).unwrap(), vec![va]);
    assert_eq!(cb.read(|tx| tx.vm_ids()).unwrap(), vec![vb]);
}

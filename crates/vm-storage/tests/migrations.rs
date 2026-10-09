//! Migrazioni: da zero, da versione precedente con backup, rifiuto del downgrade.

mod common;

use rusqlite::params;
use vm_storage::migrate::{self, LATEST_VERSION};
use vm_storage::{Catalog, DB_FILE, StorageError};

#[test]
fn fresh_catalog_reaches_latest_without_backup() {
    let dir = common::tempdir();
    let cat = Catalog::open(dir.path()).unwrap();
    let rep = cat.open_report().unwrap();
    assert_eq!(rep.migration.from_version, 0);
    assert_eq!(rep.migration.to_version, LATEST_VERSION);
    assert!(rep.migration.backup.is_none());
    assert_eq!(cat.schema_version().unwrap(), LATEST_VERSION);
    for d in ["media", "vms", "runs", "tmp"] {
        assert!(dir.path().join(d).is_dir(), "{d}");
    }
    drop(cat);

    // Pragmi richiesti su una connessione nuova.
    let conn = migrate::open_connection(&dir.path().join(DB_FILE)).unwrap();
    let mode: String = conn
        .query_row("PRAGMA journal_mode", [], |r| r.get(0))
        .unwrap();
    assert_eq!(mode.to_lowercase(), "wal");
    let fk: i64 = conn
        .query_row("PRAGMA foreign_keys", [], |r| r.get(0))
        .unwrap();
    assert_eq!(fk, 1);
    let n: i64 = conn
        .query_row("SELECT count(*) FROM schema_version", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n, i64::from(LATEST_VERSION));

    // Riapertura: nessuna migrazione, nessun backup.
    let cat = Catalog::open(dir.path()).unwrap();
    let rep = cat.open_report().unwrap();
    assert_eq!(rep.migration.from_version, LATEST_VERSION);
    assert!(rep.migration.backup.is_none());
    assert!(rep.previous_start.is_some());
}

#[test]
fn upgrade_from_v1_creates_dated_backup_and_keeps_data() {
    let dir = common::tempdir();
    let db = dir.path().join(DB_FILE);
    {
        let mut conn = migrate::open_connection(&db).unwrap();
        let r = migrate::migrate_to(&mut conn, &db, 1).unwrap();
        assert_eq!((r.from_version, r.to_version), (0, 1));
        conn.execute(
            "INSERT INTO vms(vm_id, name, guest_os, config_version, config_json, created_at, updated_at)
             VALUES ('0190f7a4-0000-7000-8000-000000000001', 'legacy', 'ubuntu', 1, '{}', 'x', 'x')",
            [],
        )
        .unwrap();
        // Il disco v1 non ha `allocated_bytes`.
        conn.execute(
            "INSERT INTO disks(disk_id, vm_id, role, virtual_size_bytes, managed_path, created_at)
             VALUES ('0190f7a4-0000-7000-8000-000000000002', '0190f7a4-0000-7000-8000-000000000001',
                     'primary', 1024, 'vms/0190f7a4-0000-7000-8000-000000000001/primary.qcow2', 'x')",
            [],
        )
        .unwrap();
    }
    let cat = Catalog::open(dir.path()).unwrap();
    let rep = cat.open_report().unwrap().clone();
    assert_eq!(rep.migration.from_version, 1);
    assert_eq!(rep.migration.to_version, LATEST_VERSION);
    let backup = rep.migration.backup.expect("backup prima della migrazione");
    assert_eq!(backup.parent(), Some(dir.path()));
    let name = backup.file_name().unwrap().to_string_lossy().into_owned();
    assert!(name.starts_with("catalog.sqlite3.backup-v1-"), "{name}");
    assert!(name.ends_with('Z'), "nome datato UTC: {name}");
    drop(cat);

    // Il backup è un database coerente alla versione 1, con i dati.
    let b = rusqlite::Connection::open(&backup).unwrap();
    let v: i64 = b
        .query_row("SELECT max(version) FROM schema_version", [], |r| r.get(0))
        .unwrap();
    assert_eq!(v, 1);
    let n: i64 = b
        .query_row("SELECT count(*) FROM vms WHERE name = 'legacy'", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 1);

    // Dati conservati e nuova colonna con valore predefinito.
    let conn = migrate::open_connection(&db).unwrap();
    let alloc: i64 = conn
        .query_row("SELECT allocated_bytes FROM disks", [], |r| r.get(0))
        .unwrap();
    assert_eq!(alloc, 0);
}

#[test]
fn newer_schema_is_refused_without_changes() {
    let dir = common::tempdir();
    drop(Catalog::open(dir.path()).unwrap());
    let db = dir.path().join(DB_FILE);
    {
        let conn = migrate::open_connection(&db).unwrap();
        conn.execute(
            "INSERT INTO schema_version(version, name, sql_sha256, applied_at) VALUES (?1, 'futura', 'x', 'x')",
            params![LATEST_VERSION + 7],
        )
        .unwrap();
    }
    match Catalog::open(dir.path()) {
        Err(StorageError::SchemaTooNew { found, supported }) => {
            assert_eq!(found, LATEST_VERSION + 7);
            assert_eq!(supported, LATEST_VERSION);
        }
        other => panic!("downgrade accettato: {other:?}"),
    }
    let backups = std::fs::read_dir(dir.path())
        .unwrap()
        .filter(|e| {
            e.as_ref()
                .unwrap()
                .file_name()
                .to_string_lossy()
                .contains("backup")
        })
        .count();
    assert_eq!(backups, 0, "nessun backup né migrazione su schema più nuovo");
}

#[test]
fn tampered_migration_is_detected() {
    let dir = common::tempdir();
    drop(Catalog::open(dir.path()).unwrap());
    let conn = migrate::open_connection(&dir.path().join(DB_FILE)).unwrap();
    conn.execute(
        "UPDATE schema_version SET sql_sha256 = 'diverso' WHERE version = 1",
        [],
    )
    .unwrap();
    drop(conn);
    assert!(matches!(
        Catalog::open(dir.path()),
        Err(StorageError::MigrationMismatch { version: 1 })
    ));
}

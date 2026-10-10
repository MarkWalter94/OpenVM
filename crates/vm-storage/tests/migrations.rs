//! Migrations: from scratch, from a previous version with backup, downgrade refusal.

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

    // Required pragmas on a new connection.
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

    // Reopen: no migration, no backup.
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
        // The v1 disk has no `allocated_bytes`.
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
    let backup = rep.migration.backup.expect("backup before the migration");
    assert_eq!(backup.parent(), Some(dir.path()));
    let name = backup.file_name().unwrap().to_string_lossy().into_owned();
    assert!(name.starts_with("catalog.sqlite3.backup-v1-"), "{name}");
    assert!(name.ends_with('Z'), "UTC-dated name: {name}");
    drop(cat);

    // The backup is a consistent version 1 database, with the data.
    let b = rusqlite::Connection::open(&backup).unwrap();
    let v: i64 = b
        .query_row("SELECT max(version) FROM schema_version", [], |r| r.get(0))
        .unwrap();
    assert_eq!(v, 1);
    let n: i64 = b
        .query_row("SELECT count(*) FROM vms WHERE name = 'legacy'", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(n, 1);

    // Data preserved and new column with its default value.
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
            "INSERT INTO schema_version(version, name, sql_sha256, applied_at) VALUES (?1, 'future', 'x', 'x')",
            params![LATEST_VERSION + 7],
        )
        .unwrap();
    }
    match Catalog::open(dir.path()) {
        Err(StorageError::SchemaTooNew { found, supported }) => {
            assert_eq!(found, LATEST_VERSION + 7);
            assert_eq!(supported, LATEST_VERSION);
        }
        other => panic!("downgrade accepted: {other:?}"),
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
    assert_eq!(backups, 0, "no backup or migration on a newer schema");
}

#[test]
fn tampered_migration_is_detected() {
    let dir = common::tempdir();
    drop(Catalog::open(dir.path()).unwrap());
    let conn = migrate::open_connection(&dir.path().join(DB_FILE)).unwrap();
    conn.execute(
        "UPDATE schema_version SET sql_sha256 = 'different' WHERE version = 1",
        [],
    )
    .unwrap();
    drop(conn);
    assert!(matches!(
        Catalog::open(dir.path()),
        Err(StorageError::MigrationMismatch { version: 1 })
    ));
}

/// Schema (tables, indexes, triggers) as normalized SQL text, to compare a
/// migrated catalog with one created from scratch.
fn schema_of(conn: &rusqlite::Connection) -> Vec<(String, String, String)> {
    let mut st = conn
        .prepare(
            "SELECT type, name, coalesce(sql, '') FROM sqlite_master
             WHERE name NOT LIKE 'sqlite_%' ORDER BY type, name",
        )
        .unwrap();
    st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap()
        .map(|r| {
            let (t, n, s): (String, String, String) = r.unwrap();
            // `ALTER TABLE ... RENAME` quotes the name: irrelevant.
            (
                t,
                n,
                s.replace('"', "")
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" "),
            )
        })
        .collect()
}

#[test]
fn upgrade_from_every_previous_version_matches_a_fresh_schema() {
    let fresh_dir = common::tempdir();
    drop(Catalog::open(fresh_dir.path()).unwrap());
    let fresh = schema_of(&migrate::open_connection(&fresh_dir.path().join(DB_FILE)).unwrap());

    for from in 1..LATEST_VERSION {
        let dir = common::tempdir();
        let db = dir.path().join(DB_FILE);
        {
            let mut conn = migrate::open_connection(&db).unwrap();
            let r = migrate::migrate_to(&mut conn, &db, from).unwrap();
            assert_eq!(r.to_version, from);
            conn.execute(
                "INSERT INTO vms(vm_id, name, guest_os, config_version, config_json, created_at, updated_at)
                 VALUES ('0190f7a4-0000-7000-8000-000000000001', 'legacy', 'ubuntu', 1, '{}', 'x', 'x')",
                [],
            )
            .unwrap();
            // Already concluded intents and an open one with no effects on disk:
            // they must survive the migration and the open one must be reconciled.
            for (id, state) in [(5, "committed"), (9, "pending")] {
                conn.execute(
                    "INSERT INTO journal(intent_id, kind, state, vm_id, target_kind, target_id,
                                         temp_path, final_path, created_at, updated_at)
                     VALUES (?1, 'create_disk', ?2, '0190f7a4-0000-7000-8000-000000000001', 'disk',
                             '0190f7a4-0000-7000-8000-0000000000aa', ?3,
                             'vms/0190f7a4-0000-7000-8000-000000000001/primary.qcow2', 'x', 'x')",
                    params![id, state, format!("tmp/create_disk-{id}.part")],
                )
                .unwrap();
            }
        }
        let mut cat = Catalog::open(dir.path()).unwrap();
        let rep = cat.open_report().unwrap().clone();
        assert_eq!(rep.migration.from_version, from, "from v{from}");
        assert_eq!(rep.migration.to_version, LATEST_VERSION);
        let backup = rep.migration.backup.expect("backup before the migration");
        let name = backup.file_name().unwrap().to_string_lossy().into_owned();
        assert!(
            name.starts_with(&format!("catalog.sqlite3.backup-v{from}-")),
            "{name}"
        );
        // Open intent reconciled after the migration.
        assert_eq!(rep.recovery.intents.len(), 1, "v{from}: {:?}", rep.recovery);
        assert_eq!(rep.recovery.intents[0].intent_id, 9);
        assert_eq!(
            cat.intent_state(5).unwrap(),
            vm_storage::IntentState::Committed
        );
        // Data preserved.
        let vms = cat.read(|tx| tx.vm_ids()).unwrap();
        assert_eq!(vms.len(), 1);
        // The intent sequence continues past the migrated identifiers.
        let i = cat
            .begin_intent(vm_storage::IntentSpec {
                kind: vm_storage::IntentKind::CreateDisk,
                vm_id: Some(vms[0]),
                operation_id: None,
                target_kind: vm_storage::TargetKind::Disk,
                target_id: Some(vm_storage::DiskId::new_v7().to_string()),
                final_path: Some(vm_storage::CatalogLayout::primary_disk(&vms[0])),
                read_only: false,
            })
            .unwrap();
        assert!(i.id > 9, "v{from}: new intent {}", i.id);
        cat.abort(&i).unwrap();
        drop(cat);
        let upgraded = schema_of(&migrate::open_connection(&db).unwrap());
        assert_eq!(
            upgraded, fresh,
            "schema migrated from v{from} differs from the fresh one"
        );
    }
}

#[test]
fn needs_review_state_requires_the_latest_schema() {
    // The `needs_review` state does not exist before v3: the CHECK rejects it.
    let dir = common::tempdir();
    let db = dir.path().join(DB_FILE);
    let mut conn = migrate::open_connection(&db).unwrap();
    migrate::migrate_to(&mut conn, &db, 2).unwrap();
    let insert = |c: &rusqlite::Connection| {
        c.execute(
            "INSERT INTO journal(kind, state, target_kind, temp_path, created_at, updated_at)
             VALUES ('export', 'needs_review', 'none', 'tmp/x.part', 'x', 'x')",
            [],
        )
    };
    assert!(insert(&conn).is_err());
    migrate::migrate(&mut conn, &db).unwrap();
    insert(&conn).unwrap();
}

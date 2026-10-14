//! Database opening and versioned migrations embedded in the binary.
//!
//! - `PRAGMA journal_mode=WAL`, `foreign_keys=ON`, `busy_timeout`.
//! - `schema_version` table with version, description, SHA-256 of the SQL text
//!   and application date.
//! - A database with a newer version is rejected (downgrade, section 10 F6).
//! - Backup before migrating a non-empty database: `VACUUM INTO` a file with
//!   a UTC-dated name in the same directory (section 10 F6).
//! - Update only with the VMs stopped: a migration is refused, before the
//!   backup and any write, while the catalog records runs without an end
//!   ([`StorageError::UpgradeWithOpenRuns`], section 10 F6).

use std::path::{Path, PathBuf};
use std::time::Duration;

use rusqlite::{Connection, OptionalExtension, params};
use sha2::{Digest, Sha256};

use crate::error::{Result, SqlResultExt, StorageError};
use crate::time::{UtcMillis, now_rfc3339};

/// Embedded migration.
#[derive(Debug, Clone, Copy)]
pub struct Migration {
    /// Version (1, 2, ...), contiguous.
    pub version: u32,
    /// Source file name.
    pub name: &'static str,
    /// SQL text.
    pub sql: &'static str,
}

/// Migrations in version order.
pub const MIGRATIONS: &[Migration] = &[
    Migration {
        version: 1,
        name: "0001_catalog.sql",
        sql: include_str!("../migrations/0001_catalog.sql"),
    },
    Migration {
        version: 2,
        name: "0002_storage_budget.sql",
        sql: include_str!("../migrations/0002_storage_budget.sql"),
    },
    Migration {
        version: 3,
        name: "0003_journal_review.sql",
        sql: include_str!("../migrations/0003_journal_review.sql"),
    },
    Migration {
        version: 4,
        name: "0004_operation_session.sql",
        sql: include_str!("../migrations/0004_operation_session.sql"),
    },
    Migration {
        version: 5,
        name: "0005_journal_vm_target.sql",
        sql: include_str!("../migrations/0005_journal_vm_target.sql"),
    },
    Migration {
        version: 6,
        name: "0006_baseline_state.sql",
        sql: include_str!("../migrations/0006_baseline_state.sql"),
    },
    Migration {
        version: 7,
        name: "0007_media_detected_guest.sql",
        sql: include_str!("../migrations/0007_media_detected_guest.sql"),
    },
    Migration {
        version: 8,
        name: "0008_media_in_place.sql",
        sql: include_str!("../migrations/0008_media_in_place.sql"),
    },
    Migration {
        version: 9,
        name: "0009_import_vm.sql",
        sql: include_str!("../migrations/0009_import_vm.sql"),
    },
];

/// Latest schema version known to this binary.
pub const LATEST_VERSION: u32 = MIGRATIONS[MIGRATIONS.len() - 1].version;

/// Wait timeout on SQLite locks.
pub const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

/// Outcome of opening with migration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MigrationReport {
    /// Version found when opening (0 = new database).
    pub from_version: u32,
    /// Version after the migration.
    pub to_version: u32,
    /// Backup copy created before the migration, if the database was not empty.
    pub backup: Option<PathBuf>,
}

/// Opens the connection and sets the required pragmas.
pub fn open_connection(path: &Path) -> Result<Connection> {
    let conn = Connection::open(path).db()?;
    conn.busy_timeout(BUSY_TIMEOUT).db()?;
    let mode: String = conn
        .query_row("PRAGMA journal_mode=WAL", [], |r| r.get(0))
        .db()?;
    if !mode.eq_ignore_ascii_case("wal") {
        return Err(StorageError::Constraint(format!(
            "journal_mode WAL not enabled (got {mode})"
        )));
    }
    conn.execute_batch("PRAGMA foreign_keys=ON; PRAGMA synchronous=FULL;")
        .db()?;
    let fk: i64 = conn
        .query_row("PRAGMA foreign_keys", [], |r| r.get(0))
        .db()?;
    if fk != 1 {
        return Err(StorageError::Constraint("foreign_keys not enabled".into()));
    }
    Ok(conn)
}

/// SHA-256 of a migration's SQL text with line endings normalized to LF:
/// the text is embedded at build time from a file whose line endings depend
/// on the checkout (`core.autocrlf` on Windows writes CRLF), and the hash
/// must not.
fn sql_sha256(sql: &str) -> String {
    crate::hex(&Sha256::digest(sql.replace("\r\n", "\n").as_bytes()))
}

/// Whether a stored hash matches the migration. Besides the normalized form,
/// catalogs created before normalization stored the hash of the text exactly
/// as embedded, with either line ending: both are accepted.
fn sql_sha256_matches(sql: &str, stored: &str) -> bool {
    let lf = sql.replace("\r\n", "\n");
    let crlf = lf.replace('\n', "\r\n");
    [lf, crlf]
        .iter()
        .any(|t| crate::hex(&Sha256::digest(t.as_bytes())) == stored)
}

fn ensure_version_table(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS schema_version (
             version     INTEGER PRIMARY KEY NOT NULL,
             name        TEXT NOT NULL,
             sql_sha256  TEXT NOT NULL,
             applied_at  TEXT NOT NULL
         ) STRICT;",
    )
    .db()
}

/// Current schema version (0 if the database is new).
pub fn current_version(conn: &Connection) -> Result<u32> {
    let exists: Option<i64> = conn
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type='table' AND name='schema_version'",
            [],
            |r| r.get(0),
        )
        .optional()
        .db()?;
    if exists.is_none() {
        return Ok(0);
    }
    let v: Option<i64> = conn
        .query_row("SELECT max(version) FROM schema_version", [], |r| r.get(0))
        .db()?;
    Ok(u32::try_from(v.unwrap_or(0)).unwrap_or(u32::MAX))
}

/// Brings the database to the latest version. Rejects newer versions and
/// migrations applied with different text. Creates a backup if the database
/// already contains a schema and at least one migration is needed.
pub fn migrate(conn: &mut Connection, db_path: &Path) -> Result<MigrationReport> {
    migrate_to(conn, db_path, LATEST_VERSION)
}

/// Like [`migrate`], stopping at `target` (used by tests to simulate a
/// catalog created by an earlier version).
#[doc(hidden)]
pub fn migrate_to(conn: &mut Connection, db_path: &Path, target: u32) -> Result<MigrationReport> {
    let from = current_version(conn)?;
    if from > LATEST_VERSION {
        return Err(StorageError::SchemaTooNew {
            found: from,
            supported: LATEST_VERSION,
        });
    }
    verify_applied(conn, from)?;
    let target = target.min(LATEST_VERSION);
    if from >= target {
        return Ok(MigrationReport {
            from_version: from,
            to_version: from,
            backup: None,
        });
    }
    // Update only with the VMs stopped (ROADMAP 10 F6): a run without an end
    // means a VM of the previous version may still be running (or crashed
    // without reconciliation). Refused before the backup and any write.
    if from > 0 {
        let open_runs: i64 = conn
            .query_row(
                "SELECT count(*) FROM runs WHERE ended_at IS NULL",
                [],
                |r| r.get(0),
            )
            .db()?;
        if open_runs > 0 {
            return Err(StorageError::UpgradeWithOpenRuns {
                from,
                to: target,
                open_runs: u32::try_from(open_runs).unwrap_or(u32::MAX),
            });
        }
    }
    let backup = if from > 0 {
        Some(backup_database(conn, db_path, from)?)
    } else {
        None
    };
    ensure_version_table(conn)?;
    // Foreign keys off while the migrations run (the pragma has no effect
    // inside a transaction): a migration that rebuilds a referenced table
    // (0008, `media`) drops the old one, which with the keys on is an
    // implicit DELETE refused by `ON DELETE RESTRICT`. This is SQLite's
    // documented procedure for table rebuilds; the integrity of every
    // reference is checked right after (`foreign_key_check`).
    conn.execute_batch("PRAGMA foreign_keys=OFF;").db()?;
    let applied = apply_migrations(conn, from, target);
    conn.execute_batch("PRAGMA foreign_keys=ON;").db()?;
    applied?;
    let fk_violations: i64 = conn
        .query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |r| {
            r.get(0)
        })
        .db()?;
    if fk_violations != 0 {
        return Err(StorageError::Constraint(format!(
            "{fk_violations} foreign key violations after the migration"
        )));
    }
    Ok(MigrationReport {
        from_version: from,
        to_version: target,
        backup,
    })
}

/// Applies the migrations after `from` up to `target`, each in its own
/// transaction with its `schema_version` row.
fn apply_migrations(conn: &mut Connection, from: u32, target: u32) -> Result<()> {
    for m in MIGRATIONS
        .iter()
        .filter(|m| m.version > from && m.version <= target)
    {
        let tx = conn.transaction().db()?;
        tx.execute_batch(m.sql).db()?;
        tx.execute(
            "INSERT INTO schema_version(version, name, sql_sha256, applied_at) VALUES (?1, ?2, ?3, ?4)",
            params![m.version, m.name, sql_sha256(m.sql), now_rfc3339()],
        )
        .db()?;
        tx.commit().db()?;
    }
    Ok(())
}

fn verify_applied(conn: &Connection, from: u32) -> Result<()> {
    if from == 0 {
        return Ok(());
    }
    let mut stmt = conn
        .prepare("SELECT version, sql_sha256 FROM schema_version ORDER BY version")
        .db()?;
    let rows = stmt
        .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))
        .db()?;
    for row in rows {
        let (v, sha) = row.db()?;
        let v = u32::try_from(v).unwrap_or(u32::MAX);
        match MIGRATIONS.iter().find(|m| m.version == v) {
            Some(m) if sql_sha256_matches(m.sql, &sha) => {}
            _ => return Err(StorageError::MigrationMismatch { version: v }),
        }
    }
    Ok(())
}

/// Consistent copy of the database (`VACUUM INTO`) next to the original, named
/// `catalog.sqlite3.backup-v<version>-<compact UTC>[-n]`. Does not overwrite.
pub fn backup_database(conn: &Connection, db_path: &Path, version: u32) -> Result<PathBuf> {
    let dir = db_path.parent().unwrap_or_else(|| Path::new("."));
    let base = db_path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| crate::layout::DB_FILE.to_owned());
    let stamp = UtcMillis::now().compact();
    let mut candidate = dir.join(format!("{base}.backup-v{version}-{stamp}"));
    let mut n = 1;
    while candidate.exists() {
        candidate = dir.join(format!("{base}.backup-v{version}-{stamp}-{n}"));
        n += 1;
    }
    let target = candidate.to_string_lossy().into_owned();
    conn.execute("VACUUM INTO ?1", params![target]).db()?;
    Ok(candidate)
}

#[cfg(test)]
mod tests {
    use sha2::Digest as _;

    #[test]
    fn migration_hash_ignores_line_endings() {
        let lf = "CREATE TABLE t (a);\nCREATE TABLE u (b);\n";
        let crlf = lf.replace('\n', "\r\n");
        assert_eq!(super::sql_sha256(lf), super::sql_sha256(&crlf));
        // Catalogs written before the normalization stored the raw hash of
        // either form: both still open.
        let raw = |t: &str| crate::hex(&sha2::Sha256::digest(t.as_bytes()));
        assert!(super::sql_sha256_matches(lf, &raw(&crlf)));
        assert!(super::sql_sha256_matches(&crlf, &raw(lf)));
        assert!(!super::sql_sha256_matches(lf, &raw("other")));
    }
}

//! Apertura del database e migrazioni versionate incorporate nel binario.
//!
//! - `PRAGMA journal_mode=WAL`, `foreign_keys=ON`, `busy_timeout`.
//! - Tabella `schema_version` con versione, descrizione, SHA-256 del testo SQL
//!   e data di applicazione.
//! - Rifiuto di un database con versione più recente (downgrade, sezione 10 F6).
//! - Backup prima di migrare un database non vuoto: `VACUUM INTO` in un file
//!   con nome datato UTC nella stessa directory (sezione 10 F6).

use std::path::{Path, PathBuf};
use std::time::Duration;

use rusqlite::{Connection, OptionalExtension, params};
use sha2::{Digest, Sha256};

use crate::error::{Result, SqlResultExt, StorageError};
use crate::time::{UtcMillis, now_rfc3339};

/// Migrazione incorporata.
#[derive(Debug, Clone, Copy)]
pub struct Migration {
    /// Versione (1, 2, ...), contigua.
    pub version: u32,
    /// Nome del file sorgente.
    pub name: &'static str,
    /// Testo SQL.
    pub sql: &'static str,
}

/// Migrazioni in ordine di versione.
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
];

/// Versione di schema più recente nota a questo binario.
pub const LATEST_VERSION: u32 = MIGRATIONS[MIGRATIONS.len() - 1].version;

/// Timeout di attesa sui lock SQLite.
pub const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

/// Esito dell'apertura con migrazione.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MigrationReport {
    /// Versione trovata all'apertura (0 = database nuovo).
    pub from_version: u32,
    /// Versione dopo la migrazione.
    pub to_version: u32,
    /// Copia di sicurezza creata prima della migrazione, se il database non era vuoto.
    pub backup: Option<PathBuf>,
}

/// Apre la connessione e imposta i pragma richiesti.
pub fn open_connection(path: &Path) -> Result<Connection> {
    let conn = Connection::open(path).db()?;
    conn.busy_timeout(BUSY_TIMEOUT).db()?;
    let mode: String = conn
        .query_row("PRAGMA journal_mode=WAL", [], |r| r.get(0))
        .db()?;
    if !mode.eq_ignore_ascii_case("wal") {
        return Err(StorageError::Constraint(format!(
            "journal_mode WAL non attivato (ottenuto {mode})"
        )));
    }
    conn.execute_batch("PRAGMA foreign_keys=ON; PRAGMA synchronous=FULL;")
        .db()?;
    let fk: i64 = conn
        .query_row("PRAGMA foreign_keys", [], |r| r.get(0))
        .db()?;
    if fk != 1 {
        return Err(StorageError::Constraint("foreign_keys non attivo".into()));
    }
    Ok(conn)
}

fn sql_sha256(sql: &str) -> String {
    crate::hex(&Sha256::digest(sql.as_bytes()))
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

/// Versione di schema corrente (0 se il database è nuovo).
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

/// Porta il database all'ultima versione. Rifiuta versioni più recenti e
/// migrazioni applicate con testo diverso. Crea un backup se il database
/// contiene già uno schema e serve almeno una migrazione.
pub fn migrate(conn: &mut Connection, db_path: &Path) -> Result<MigrationReport> {
    migrate_to(conn, db_path, LATEST_VERSION)
}

/// Come [`migrate`], fermandosi a `target` (usato dai test per simulare un
/// catalogo creato da una versione precedente).
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
    let backup = if from > 0 {
        Some(backup_database(conn, db_path, from)?)
    } else {
        None
    };
    ensure_version_table(conn)?;
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
    let fk_violations: i64 = conn
        .query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |r| {
            r.get(0)
        })
        .db()?;
    if fk_violations != 0 {
        return Err(StorageError::Constraint(format!(
            "{fk_violations} violazioni di chiave esterna dopo la migrazione"
        )));
    }
    Ok(MigrationReport {
        from_version: from,
        to_version: target,
        backup,
    })
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
            Some(m) if sql_sha256(m.sql) == sha => {}
            _ => return Err(StorageError::MigrationMismatch { version: v }),
        }
    }
    Ok(())
}

/// Copia coerente del database (`VACUUM INTO`) accanto all'originale, con nome
/// `catalog.sqlite3.backup-v<versione>-<UTC compatto>[-n]`. Non sovrascrive.
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

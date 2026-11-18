//! Standing grant rows (ADR-0039, migration 0012).
//!
//! The catalog keeps the policy of the grant (opaque JSON for this crate),
//! its state and the SHA-256 of its pairing credential: never the
//! credential. At most one row is live (active or suspended), enforced by a
//! partial unique index. Deleting the VM revokes its live grant in the same
//! transaction ([`crate::Catalog::delete_vm_with_files`]).

use rusqlite::{OptionalExtension, params};

use crate::error::{Result, SqlResultExt, StorageError};
use crate::ids::{IdSql, VmId, idc};
use crate::records::Tx;
use crate::time::now_rfc3339;

/// State column of a standing grant.
pub const STATE_ACTIVE: &str = "active";
/// State column of a suspended standing grant.
pub const STATE_SUSPENDED: &str = "suspended";
/// State column of a revoked standing grant.
pub const STATE_REVOKED: &str = "revoked";
/// State column of an expired standing grant.
pub const STATE_EXPIRED: &str = "expired";
/// Revocation cause written when the VM of the grant is deleted.
pub const CAUSE_VM_DELETED: &str = "vm_deleted";

/// A standing grant row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StandingGrantRow {
    /// Identifier (UUID text).
    pub grant_id: String,
    /// VM of the grant (catalog identifier).
    pub vm_id: VmId,
    /// SHA-256 of the pairing credential, 64 lowercase hex digits.
    pub credential_sha256: String,
    /// `active`, `suspended`, `revoked` or `expired`.
    pub state: String,
    /// Revocation cause, if revoked.
    pub state_cause: Option<String>,
    /// Instant of the revocation or of the expiry (RFC 3339).
    pub state_at: Option<String>,
    /// Policy (JSON object, opaque here).
    pub policy_json: String,
    /// Creation instant (RFC 3339).
    pub created_at: String,
}

impl StandingGrantRow {
    fn live(&self) -> bool {
        self.state == STATE_ACTIVE || self.state == STATE_SUSPENDED
    }

    fn check(&self) -> Result<()> {
        if !matches!(
            self.state.as_str(),
            STATE_ACTIVE | STATE_SUSPENDED | STATE_REVOKED | STATE_EXPIRED
        ) {
            return Err(StorageError::Constraint("standing grant state".into()));
        }
        if self.credential_sha256.len() != 64
            || !self
                .credential_sha256
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(StorageError::Constraint(
                "standing grant credential hash".into(),
            ));
        }
        Ok(())
    }
}

fn row(r: &rusqlite::Row<'_>) -> rusqlite::Result<StandingGrantRow> {
    Ok(StandingGrantRow {
        grant_id: r.get(0)?,
        vm_id: idc(r, 1)?,
        credential_sha256: r.get(2)?,
        state: r.get(3)?,
        state_cause: r.get(4)?,
        state_at: r.get(5)?,
        policy_json: r.get(6)?,
        created_at: r.get(7)?,
    })
}

const COLUMNS: &str =
    "grant_id, vm_id, credential_sha256, state, state_cause, state_at, policy_json, created_at";

impl Tx<'_> {
    /// Records a new standing grant. Refused with
    /// [`StorageError::StateConflict`] while another grant is live.
    pub fn insert_standing_grant(&self, g: &StandingGrantRow) -> Result<()> {
        g.check()?;
        if g.live() && self.live_standing_grant()?.is_some() {
            return Err(StorageError::StateConflict(
                "a standing grant is already live".into(),
            ));
        }
        let now = now_rfc3339();
        self.conn()
            .execute(
                &format!(
                    "INSERT INTO standing_grants({COLUMNS}, live, updated_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)"
                ),
                params![
                    g.grant_id,
                    g.vm_id.sql(),
                    g.credential_sha256,
                    g.state,
                    g.state_cause,
                    g.state_at,
                    g.policy_json,
                    g.created_at,
                    i64::from(g.live()),
                    now
                ],
            )
            .db()?;
        Ok(())
    }

    /// Updates state and policy of a standing grant (the credential hash,
    /// the VM and the creation instant never change). A grant that is no
    /// longer live never becomes live again.
    pub fn update_standing_grant(&self, g: &StandingGrantRow) -> Result<()> {
        g.check()?;
        let now = now_rfc3339();
        let n = self
            .conn()
            .execute(
                "UPDATE standing_grants
                    SET state = ?2, state_cause = ?3, state_at = ?4, policy_json = ?5,
                        live = ?6, updated_at = ?7
                  WHERE grant_id = ?1 AND (live = 1 OR ?6 = 0)",
                params![
                    g.grant_id,
                    g.state,
                    g.state_cause,
                    g.state_at,
                    g.policy_json,
                    i64::from(g.live()),
                    now
                ],
            )
            .db()?;
        if n == 0 {
            return Err(StorageError::NotFound {
                kind: "standing_grant",
                id: g.grant_id.clone(),
            });
        }
        Ok(())
    }

    /// The live standing grant, if any.
    pub fn live_standing_grant(&self) -> Result<Option<StandingGrantRow>> {
        self.conn()
            .query_row(
                &format!("SELECT {COLUMNS} FROM standing_grants WHERE live = 1"),
                [],
                row,
            )
            .optional()
            .db()
    }

    /// A standing grant by identifier.
    pub fn standing_grant(&self, grant_id: &str) -> Result<Option<StandingGrantRow>> {
        self.conn()
            .query_row(
                &format!("SELECT {COLUMNS} FROM standing_grants WHERE grant_id = ?1"),
                params![grant_id],
                row,
            )
            .optional()
            .db()
    }

    /// Baselines created by sessions of a standing grant.
    pub fn standing_grant_baselines(&self, grant_id: &str) -> Result<Vec<String>> {
        let mut st = self
            .conn()
            .prepare(
                "SELECT baseline_id FROM standing_grant_baselines WHERE grant_id = ?1
                 ORDER BY baseline_id",
            )
            .db()?;
        let rows = st
            .query_map(params![grant_id], |r| r.get::<_, String>(0))
            .db()?;
        rows.collect::<rusqlite::Result<Vec<_>>>().db()
    }

    /// Records a baseline created by a session of a standing grant.
    pub fn add_standing_grant_baseline(&self, grant_id: &str, baseline_id: &str) -> Result<()> {
        self.conn()
            .execute(
                "INSERT OR IGNORE INTO standing_grant_baselines(grant_id, baseline_id)
                 VALUES (?1, ?2)",
                params![grant_id, baseline_id],
            )
            .db()?;
        Ok(())
    }

    /// Revokes the live standing grant of a VM (deletion of the VM, same
    /// transaction). Returns the number of grants revoked.
    pub(crate) fn revoke_standing_grants_of_vm(&self, vm: &VmId) -> Result<usize> {
        let now = now_rfc3339();
        self.conn()
            .execute(
                "UPDATE standing_grants
                    SET state = ?2, state_cause = ?3, state_at = ?4, live = 0, updated_at = ?4
                  WHERE vm_id = ?1 AND live = 1",
                params![vm.sql(), STATE_REVOKED, CAUSE_VM_DELETED, now],
            )
            .db()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Catalog;

    fn grant(id: &str, vm: VmId, state: &str) -> StandingGrantRow {
        StandingGrantRow {
            grant_id: id.into(),
            vm_id: vm,
            credential_sha256: "ab".repeat(32),
            state: state.into(),
            state_cause: None,
            state_at: None,
            policy_json: "{}".into(),
            created_at: now_rfc3339(),
        }
    }

    fn vm(cat: &mut Catalog) -> VmId {
        cat.with_tx(|tx| {
            tx.insert_vm(&crate::NewVm {
                name: format!("vm-{}", VmId::new_v7()),
                guest_os: "linux".into(),
                config_version: 1,
                config: serde_json::json!({}),
            })
        })
        .unwrap()
    }

    #[test]
    fn one_live_grant_and_terminal_states_stay_terminal() {
        let dir = tempfile::tempdir().unwrap();
        let mut cat = Catalog::open(dir.path()).unwrap();
        let v = vm(&mut cat);
        cat.with_tx(|tx| tx.insert_standing_grant(&grant("a", v, STATE_ACTIVE)))
            .unwrap();
        // A second live grant is refused.
        let e = cat
            .with_tx(|tx| tx.insert_standing_grant(&grant("b", v, STATE_ACTIVE)))
            .unwrap_err();
        assert!(matches!(e, StorageError::StateConflict(_)), "{e:?}");
        let live = cat.read(|tx| tx.live_standing_grant()).unwrap().unwrap();
        assert_eq!(live.grant_id, "a");
        // Suspended is still live.
        let mut g = live.clone();
        g.state = STATE_SUSPENDED.into();
        cat.with_tx(|tx| tx.update_standing_grant(&g)).unwrap();
        assert!(cat.read(|tx| tx.live_standing_grant()).unwrap().is_some());
        // Expired: no longer live, never live again.
        g.state = STATE_EXPIRED.into();
        g.state_at = Some(now_rfc3339());
        cat.with_tx(|tx| tx.update_standing_grant(&g)).unwrap();
        assert!(cat.read(|tx| tx.live_standing_grant()).unwrap().is_none());
        g.state = STATE_ACTIVE.into();
        assert!(cat.with_tx(|tx| tx.update_standing_grant(&g)).is_err());
        // A new one can be created now.
        cat.with_tx(|tx| tx.insert_standing_grant(&grant("b", v, STATE_ACTIVE)))
            .unwrap();
        // Bad hash refused.
        let mut bad = grant("c", v, STATE_REVOKED);
        bad.credential_sha256 = "x".into();
        assert!(cat.with_tx(|tx| tx.insert_standing_grant(&bad)).is_err());
    }

    #[test]
    fn baselines_of_a_grant() {
        let dir = tempfile::tempdir().unwrap();
        let mut cat = Catalog::open(dir.path()).unwrap();
        let v = vm(&mut cat);
        cat.with_tx(|tx| tx.insert_standing_grant(&grant("a", v, STATE_ACTIVE)))
            .unwrap();
        cat.with_tx(|tx| {
            tx.add_standing_grant_baseline("a", "b2")?;
            tx.add_standing_grant_baseline("a", "b1")?;
            tx.add_standing_grant_baseline("a", "b1")
        })
        .unwrap();
        assert_eq!(
            cat.read(|tx| tx.standing_grant_baselines("a")).unwrap(),
            vec!["b1".to_string(), "b2".to_string()]
        );
        assert!(
            cat.read(|tx| tx.standing_grant_baselines("z"))
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn deleting_the_vm_revokes_its_grant_in_the_same_transaction() {
        let dir = tempfile::tempdir().unwrap();
        let mut cat = Catalog::open(dir.path()).unwrap();
        let v = vm(&mut cat);
        cat.with_tx(|tx| tx.insert_standing_grant(&grant("a", v, STATE_ACTIVE)))
            .unwrap();
        cat.delete_vm_with_files(&v, &[]).unwrap();
        assert!(cat.read(|tx| tx.live_standing_grant()).unwrap().is_none());
        let g = cat.read(|tx| tx.standing_grant("a")).unwrap().unwrap();
        assert_eq!(g.state, STATE_REVOKED);
        assert_eq!(g.state_cause.as_deref(), Some(CAUSE_VM_DELETED));
        assert!(g.state_at.is_some());
    }
}

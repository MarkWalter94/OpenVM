//! Offline checkpoints of the file-backed swtpm permanent state.
//!
//! Only the plain TPM 2 file backend used by LightVM is supported. Refuse
//! additional state files (including volatile/save state), directories and
//! links instead of silently omitting part of an emulator's identity.
use std::{fs::File, path::PathBuf};

use rusqlite::{OptionalExtension, params};

use crate::{
    Catalog, CatalogLayout, FirmwareRecord, ManagedPath,
    error::{Result, SqlResultExt, StorageError},
    fsx::{self, Share},
    ids::{BaselineId, FirmwareId, IdSql, SnapshotId, VmId, idc},
    records::Tx,
};

pub(crate) const PERMANENT: &str = "tpm2-00.permall";
pub(crate) const TPM_SNAPSHOT_RUNNING: &str =
    "TPM snapshots require the VM to be stopped so disk, UEFI and TPM state are consistent";

pub(crate) fn needs_tpm(os: &str) -> bool {
    serde_json::from_value::<lightvm_proto::GuestOs>(serde_json::Value::String(os.into()))
        .is_ok_and(lightvm_proto::GuestOs::needs_tpm)
}

pub(crate) fn state_file(dir: &ManagedPath) -> ManagedPath {
    ManagedPath::parse(&format!("{dir}/{PERMANENT}")).expect("bounded catalog TPM path")
}

/// `true` for the permanent state of a per-overlay emulator directory
/// (`vms/<vm>/tpm/<disk_id>/tpm2-00.permall`,
/// [`CatalogLayout::state_tpm_dir`]): that directory lies inside the
/// standalone state directory, which accepts nothing but its own state
/// ([`Catalog::open_tpm_state`]), so it must not outlive its state.
pub(crate) fn is_state_tpm_file(rel: &ManagedPath) -> bool {
    let c: Vec<&str> = rel.components().collect();
    matches!(c.as_slice(), ["vms", _, "tpm", _, PERMANENT])
}

impl Tx<'_> {
    pub(crate) fn current_tpm(&self, id: &VmId) -> Result<Option<FirmwareRecord>> {
        let fw: Option<FirmwareId> = self
            .conn()
            .query_row(
                "SELECT fw_id FROM current_tpm WHERE vm_id = ?1",
                params![id.sql()],
                |r| idc(r, 0),
            )
            .optional()
            .db()?;
        fw.map(|id| self.firmware(&id)).transpose()
    }

    pub(crate) fn set_current_tpm(&self, vm: &VmId, fw: &FirmwareId) -> Result<()> {
        self.conn().execute("INSERT INTO current_tpm(vm_id,fw_id) VALUES (?1,?2) ON CONFLICT(vm_id) DO UPDATE SET fw_id=excluded.fw_id",
            params![vm.sql(),fw.sql()]).db()?;
        Ok(())
    }
    pub(crate) fn baseline_tpm(&self, id: &BaselineId) -> Result<Option<FirmwareRecord>> {
        let fw: Option<FirmwareId> = self
            .conn()
            .query_row(
                "SELECT fw_id FROM baseline_tpm WHERE baseline_id = ?1",
                params![id.sql()],
                |r| idc(r, 0),
            )
            .optional()
            .db()?;
        fw.map(|id| self.firmware(&id)).transpose()
    }

    pub(crate) fn set_baseline_tpm(&self, id: &BaselineId, fw: &FirmwareId) -> Result<()> {
        self.conn()
            .execute(
                "INSERT INTO baseline_tpm(baseline_id, fw_id) VALUES (?1, ?2)",
                params![id.sql(), fw.sql()],
            )
            .db()?;
        Ok(())
    }

    pub(crate) fn snapshot_tpm(&self, id: &SnapshotId) -> Result<Option<(String, u64)>> {
        self.conn()
            .query_row(
                "SELECT sha256,size_bytes FROM snapshot_tpm WHERE snapshot_id=?1",
                params![id.sql()],
                |r| Ok((r.get(0)?, r.get::<_, i64>(1)? as u64)),
            )
            .optional()
            .db()
    }

    pub(crate) fn set_snapshot_tpm(&self, id: &SnapshotId, hash: &str, bytes: u64) -> Result<()> {
        self.conn()
            .execute(
                "INSERT INTO snapshot_tpm(snapshot_id,sha256,size_bytes) VALUES (?1,?2,?3)",
                params![id.sql(), hash, bytes as i64],
            )
            .db()?;
        Ok(())
    }
}

impl Catalog {
    /// Current emulator directory: tied to the overlay identity after a
    /// baseline restore, so publication never replaces a live old state.
    pub fn current_tpm_dir(&mut self, vm: &VmId) -> Result<ManagedPath> {
        Ok(match self.read(|tx| tx.current_state(vm))? {
            Some(s) => CatalogLayout::state_tpm_dir(vm, &s.overlay_disk_id),
            None => CatalogLayout::tpm_state_dir(vm),
        })
    }

    /// Removes the empty per-overlay emulator directories
    /// (`vms/<vm>/tpm/<disk_id>/`) left by a restore that did not commit
    /// (catalogs written before its rollback removed them) or by a state
    /// that was replaced: inside the standalone state directory they make
    /// [`Self::open_tpm_state`] refuse it. The current state's directory and
    /// those named by unfinished intents are kept; links, files and
    /// non-empty directories are never touched. Best effort: run by
    /// reconciliation, it never prevents opening the catalog.
    pub(crate) fn remove_empty_tpm_dirs(&mut self) {
        let open: Vec<String> = {
            let Ok(mut st) = self.conn.prepare(
                "SELECT final_path FROM journal
                 WHERE state IN ('pending','applied','needs_review') AND final_path IS NOT NULL",
            ) else {
                return;
            };
            let Ok(rows) = st.query_map([], |r| r.get::<_, String>(0)) else {
                return;
            };
            let Ok(v) = rows.collect::<rusqlite::Result<_>>() else {
                return;
            };
            v
        };
        let Ok(vms) = std::fs::read_dir(self.layout.root().join("vms")) else {
            return;
        };
        for vm_entry in vms.flatten() {
            let Some(vm) = vm_entry
                .file_name()
                .to_str()
                .and_then(|n| n.parse::<VmId>().ok())
            else {
                continue;
            };
            let current = match self.read(|tx| tx.current_state(&vm)) {
                Ok(c) => c.map(|c| c.overlay_disk_id.to_string()),
                Err(_) => continue,
            };
            let rel = CatalogLayout::tpm_state_dir(&vm);
            let Ok(abs) = self.layout.resolve(&rel) else {
                continue;
            };
            let Ok(entries) = std::fs::read_dir(&abs) else {
                continue;
            };
            for entry in entries.flatten() {
                let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                    continue;
                };
                if current.as_deref() == Some(name.as_str()) {
                    continue;
                }
                let Ok(child) = rel.join(&name) else {
                    continue;
                };
                let prefix = format!("{child}/");
                if open.iter().any(|f| f.starts_with(&prefix)) {
                    continue;
                }
                let p = entry.path();
                let is_dir = std::fs::symlink_metadata(&p)
                    .is_ok_and(|m| !crate::paths::is_link(&m) && m.is_dir());
                if is_dir
                    && let Ok(mut it) = std::fs::read_dir(&p)
                    && it.next().is_none()
                {
                    let _ = std::fs::remove_dir(&p);
                }
            }
        }
    }

    pub(crate) fn open_tpm_state(&self, dir: &ManagedPath) -> Result<(PathBuf, File)> {
        let abs = self.layout.resolve(dir)?;
        let entries = std::fs::read_dir(&abs).map_err(|e| StorageError::io(&abs, e))?;
        for entry in entries {
            let entry = entry.map_err(|e| StorageError::io(&abs, e))?;
            let name = entry.file_name();
            let p = entry.path();
            let meta = std::fs::symlink_metadata(&p).map_err(|e| StorageError::io(&p, e))?;
            if crate::paths::is_link(&meta)
                || !meta.is_file()
                || (name != PERMANENT && name != ".lock")
            {
                return Err(StorageError::StateConflict(
                    "unsupported TPM state layout".into(),
                ));
            }
        }
        let rel = state_file(dir);
        let abs = self.layout.resolve(&rel)?;
        let f = fsx::open_regular(&abs, Share::ReadOnly, rel.as_str())?;
        let len = f.metadata().map_err(|e| StorageError::io(&abs, e))?.len();
        if len == 0 || len > 16 << 20 {
            return Err(StorageError::StateConflict(
                "invalid TPM permanent state size".into(),
            ));
        }
        Ok((abs, f))
    }
}

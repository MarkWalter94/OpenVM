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

//! Application-level storage budget (ROADMAP 7.2 item 8, section 8, ADR-0017).
//!
//! Nature of the check: an **application budget** computed from the bytes
//! recorded in the catalog (measured disks, firmware, artifacts, logs, media)
//! plus the metadata files, checked before a new run is accepted. It is not a
//! hard operating-system quota: between one check and the next, usage may
//! exceed the budget, and nothing protects against the volume being filled by
//! other processes. A policy that requires a hard OS quota is rejected with
//! [`StorageError::Unsupported`].

use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};

use crate::catalog::Catalog;
use crate::error::{Result, SqlResultExt, StorageError};
use crate::ids::{BaselineId, IdSql, RunId, VmId, idc};
use crate::time::now_rfc3339;

/// Nature of the check required by the policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Enforcement {
    /// Application budget, checked before start and monitored.
    ApplicationBudget,
    /// Hard operating-system quota: not implemented.
    OsHardQuota,
}

/// Usage per category, in bytes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StorageUsage {
    /// Run overlays (measured bytes).
    pub overlays: u64,
    /// Baseline base disks and their vars.
    pub baselines: u64,
    /// VM working disks and vars.
    pub primary: u64,
    /// Run vars.
    pub run_firmware: u64,
    /// Screenshots and reports.
    pub artifacts: u64,
    /// Logs.
    pub logs: u64,
    /// Managed media copies (catalog total only; files used in place,
    /// ADR-0029, are outside the catalog and not counted).
    pub media: u64,
    /// Database and auxiliary files (catalog total only).
    pub metadata: u64,
    /// Files of the user snapshots (UEFI variables and thumbnails,
    /// ADR-0032); their disk clusters live inside the VM's disk.
    #[serde(default)]
    pub snapshots: u64,
}

impl StorageUsage {
    /// Sum of all categories.
    pub fn total(&self) -> u64 {
        [
            self.overlays,
            self.baselines,
            self.primary,
            self.run_firmware,
            self.artifacts,
            self.logs,
            self.media,
            self.metadata,
            self.snapshots,
        ]
        .iter()
        .fold(0u64, |a, b| a.saturating_add(*b))
    }
}

/// Configured budgets.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BudgetConfig {
    /// Catalog budget (`None` = not declared).
    pub global_bytes: Option<u64>,
    /// Reserve to leave free on the host volume.
    pub host_reserve_bytes: u64,
    /// VM budget (`None` = not declared).
    pub vm_bytes: Option<u64>,
}

/// Successful check result, with the values used.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BudgetCheck {
    /// VM usage.
    pub vm_usage: StorageUsage,
    /// Catalog usage.
    pub catalog_usage: StorageUsage,
    /// Budgets applied.
    pub config: BudgetConfig,
    /// Estimate requested for the new run.
    pub requested_bytes: u64,
}

fn u(v: i64) -> u64 {
    u64::try_from(v).unwrap_or(0)
}

fn i(v: u64) -> Result<i64> {
    i64::try_from(v).map_err(|_| StorageError::Constraint("value out of range".into()))
}

impl Catalog {
    /// Sets the catalog budget and the host reserve.
    pub fn set_global_budget(&mut self, budget_bytes: u64, host_reserve_bytes: u64) -> Result<()> {
        let (b, r) = (i(budget_bytes)?, i(host_reserve_bytes)?);
        self.with_tx(|tx| {
            tx.conn()
                .execute(
                    "INSERT INTO storage_budgets(scope_key, vm_id, budget_bytes, host_reserve_bytes, updated_at)
                     VALUES ('global', NULL, ?1, ?2, ?3)
                     ON CONFLICT(scope_key) DO UPDATE SET budget_bytes = excluded.budget_bytes,
                         host_reserve_bytes = excluded.host_reserve_bytes, updated_at = excluded.updated_at",
                    params![b, r, now_rfc3339()],
                )
                .db()?;
            Ok(())
        })
    }

    /// Sets the budget of a VM.
    pub fn set_vm_budget(&mut self, vm: &VmId, budget_bytes: u64) -> Result<()> {
        let b = i(budget_bytes)?;
        let key = format!("vm:{vm}");
        self.with_tx(|tx| {
            tx.conn()
                .execute(
                    "INSERT INTO storage_budgets(scope_key, vm_id, budget_bytes, updated_at)
                     VALUES (?1, ?2, ?3, ?4)
                     ON CONFLICT(scope_key) DO UPDATE SET budget_bytes = excluded.budget_bytes,
                         updated_at = excluded.updated_at",
                    params![key, vm.sql(), b, now_rfc3339()],
                )
                .db()?;
            Ok(())
        })
    }

    /// Budgets applicable to a VM.
    pub fn budget_config(&self, vm: &VmId) -> Result<BudgetConfig> {
        let global: Option<(i64, i64)> = self
            .conn
            .query_row(
                "SELECT budget_bytes, host_reserve_bytes FROM storage_budgets WHERE scope_key = 'global'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .db()?;
        let vm_b: Option<i64> = self
            .conn
            .query_row(
                "SELECT budget_bytes FROM storage_budgets WHERE scope_key = ?1",
                params![format!("vm:{vm}")],
                |r| r.get(0),
            )
            .optional()
            .db()?;
        Ok(BudgetConfig {
            global_bytes: global.map(|g| u(g.0)),
            host_reserve_bytes: global.map(|g| u(g.1)).unwrap_or(0),
            vm_bytes: vm_b.map(u),
        })
    }

    /// Usage of a VM (`Some`) or of the whole catalog (`None`, includes media
    /// and metadata).
    pub fn storage_usage(&self, vm: Option<&VmId>) -> Result<StorageUsage> {
        let vm_s = vm.map(VmId::to_string);
        let sum = |sql: &str| -> Result<u64> {
            let v: i64 = self.conn.query_row(sql, params![vm_s], |r| r.get(0)).db()?;
            Ok(u(v))
        };
        let mut usage = StorageUsage {
            overlays: sum(
                "SELECT coalesce(sum(allocated_bytes),0) FROM disks
                 WHERE role = 'overlay' AND (?1 IS NULL OR vm_id = ?1)",
            )?,
            baselines: sum(
                "SELECT coalesce(sum(allocated_bytes),0) FROM disks
                 WHERE role = 'base' AND (?1 IS NULL OR vm_id = ?1)",
            )? + sum(
                "SELECT coalesce(sum(size_bytes),0) FROM firmware_state
                 WHERE scope = 'baseline' AND (?1 IS NULL OR vm_id = ?1)",
            )?,
            primary: sum(
                "SELECT coalesce(sum(allocated_bytes),0) FROM disks
                 WHERE role = 'primary' AND (?1 IS NULL OR vm_id = ?1)",
            )? + sum(
                "SELECT coalesce(sum(size_bytes),0) FROM firmware_state
                 WHERE scope = 'vm' AND (?1 IS NULL OR vm_id = ?1)",
            )?,
            run_firmware: sum(
                "SELECT coalesce(sum(size_bytes),0) FROM firmware_state
                 WHERE scope = 'run' AND (?1 IS NULL OR vm_id = ?1)",
            )?,
            artifacts: sum(
                "SELECT coalesce(sum(a.size_bytes),0) FROM artifacts a JOIN runs r ON a.run_id = r.run_id
                 WHERE a.kind <> 'log' AND (?1 IS NULL OR r.vm_id = ?1)",
            )?,
            logs: sum(
                "SELECT coalesce(sum(a.size_bytes),0) FROM artifacts a JOIN runs r ON a.run_id = r.run_id
                 WHERE a.kind = 'log' AND (?1 IS NULL OR r.vm_id = ?1)",
            )?,
            media: 0,
            metadata: 0,
            snapshots: sum(
                // The offline TPM copy of a snapshot (`snapshot_tpm`) is a
                // file of the snapshot like its UEFI variables.
                "SELECT coalesce(sum(s.vars_bytes + s.thumbnail_bytes
                                     + coalesce(t.size_bytes, 0)),0)
                 FROM snapshots s LEFT JOIN snapshot_tpm t ON t.snapshot_id = s.snapshot_id
                 WHERE (?1 IS NULL OR s.vm_id = ?1)",
            )?,
        };
        if vm.is_none() {
            let media: i64 = self
                .conn
                .query_row(
                    "SELECT coalesce(sum(size_bytes),0) FROM media WHERE storage = 'managed'",
                    [],
                    |r| r.get(0),
                )
                .db()?;
            usage.media = u(media);
            let db = self.layout.db_path();
            for suffix in ["", "-wal", "-shm"] {
                let mut p = db.clone().into_os_string();
                p.push(suffix);
                if let Ok(m) = std::fs::metadata(&p) {
                    usage.metadata = usage.metadata.saturating_add(m.len());
                }
            }
        }
        Ok(usage)
    }

    /// Updates the disks' `allocated_bytes` by measuring the files (file size;
    /// for sparse files this is the apparent size, hence an overestimate).
    pub fn refresh_disk_sizes(&mut self) -> Result<()> {
        let layout = self.layout.clone();
        self.with_tx(|tx| {
            for d in tx.all_disks()? {
                let p = layout.resolve(&d.managed_path)?;
                if let Ok(m) = std::fs::symlink_metadata(&p)
                    && m.is_file()
                {
                    tx.set_disk_allocated(&d.disk_id, m.len())?;
                }
            }
            Ok(())
        })
    }

    /// Checks whether a new run with estimated growth `requested_bytes` fits
    /// within the declared budgets and leaves the host reserve free.
    /// `host_free_bytes` is the volume's free space as measured by the caller.
    pub fn check_new_run(
        &self,
        vm: &VmId,
        requested_bytes: u64,
        host_free_bytes: u64,
        enforcement: Enforcement,
    ) -> Result<BudgetCheck> {
        if enforcement == Enforcement::OsHardQuota {
            return Err(StorageError::Unsupported(
                "hard operating-system quota not implemented",
            ));
        }
        let config = self.budget_config(vm)?;
        let vm_usage = self.storage_usage(Some(vm))?;
        let catalog_usage = self.storage_usage(None)?;
        if let Some(limit) = config.vm_bytes {
            let need = vm_usage.total().saturating_add(requested_bytes);
            if need > limit {
                return Err(StorageError::ResourceLimit(format!(
                    "VM budget exceeded: {need} > {limit} bytes"
                )));
            }
        }
        if let Some(limit) = config.global_bytes {
            let need = catalog_usage.total().saturating_add(requested_bytes);
            if need > limit {
                return Err(StorageError::ResourceLimit(format!(
                    "catalog budget exceeded: {need} > {limit} bytes"
                )));
            }
        }
        let need_free = requested_bytes.saturating_add(config.host_reserve_bytes);
        if host_free_bytes < need_free {
            return Err(StorageError::ResourceLimit(format!(
                "host reserve not met: {host_free_bytes} free, {need_free} bytes required"
            )));
        }
        Ok(BudgetCheck {
            vm_usage,
            catalog_usage,
            config,
            requested_bytes,
        })
    }
}

/// Name of the budget manifest in the catalog root.
pub const BUDGET_MANIFEST_FILE: &str = "budget.json";

/// Space used by a baseline (base disk and vars).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BaselineUsage {
    /// Baseline.
    pub baseline_id: BaselineId,
    /// Bytes of the base disk.
    pub disk_bytes: u64,
    /// Bytes of the baseline's vars.
    pub vars_bytes: u64,
    /// Total.
    pub total_bytes: u64,
}

/// Space used by a run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunUsage {
    /// Run.
    pub run_id: RunId,
    /// Run ended (overlay retained or pending deletion).
    pub ended: bool,
    /// Bytes of the overlay.
    pub overlay_bytes: u64,
    /// Bytes of the run's vars.
    pub firmware_bytes: u64,
    /// Screenshots and reports.
    pub artifact_bytes: u64,
    /// Logs.
    pub log_bytes: u64,
    /// Total.
    pub total_bytes: u64,
}

/// Space used and limit of a VM.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VmBudget {
    /// VM.
    pub vm_id: VmId,
    /// Declared budget (`None` = not declared).
    pub budget_bytes: Option<u64>,
    /// Usage per category.
    pub usage: StorageUsage,
    /// Total.
    pub total_bytes: u64,
    /// Budget declared and exceeded.
    pub exceeded: bool,
    /// Baselines of the VM.
    pub baselines: Vec<BaselineUsage>,
    /// Runs of the VM.
    pub runs: Vec<RunUsage>,
}

/// Budget manifest: space used per catalog, VM, baseline and run, with the
/// declared limits. A snapshot of the bytes recorded in the catalog (see
/// [`Catalog::refresh_disk_sizes`]); it is not a hard system quota.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BudgetManifest {
    /// Format version.
    pub format_version: u32,
    /// Always `"budget"`.
    pub kind: String,
    /// Generation time (UTC).
    pub generated_at: String,
    /// Nature of the check: always `"application_budget"`.
    pub enforcement: String,
    /// Catalog budget.
    pub global_bytes: Option<u64>,
    /// Reserve to leave free on the host volume.
    pub host_reserve_bytes: u64,
    /// Catalog usage (media and metadata included).
    pub catalog_usage: StorageUsage,
    /// Catalog total.
    pub catalog_total_bytes: u64,
    /// Catalog budget declared and exceeded.
    pub catalog_exceeded: bool,
    /// VMs, sorted by identifier.
    pub vms: Vec<VmBudget>,
}

/// Budget check request for a new run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RunBudgetRequest {
    /// Estimated growth of the run (overlay, vars, artifacts, logs).
    pub requested_bytes: u64,
    /// Volume free space as measured by the caller.
    pub host_free_bytes: u64,
    /// Nature of the check required by the policy.
    pub enforcement: Enforcement,
}

impl Catalog {
    /// Builds the budget manifest from the recorded bytes.
    pub fn budget_manifest(&mut self) -> Result<BudgetManifest> {
        let vm_ids = self.read(|tx| tx.vm_ids())?;
        let catalog_usage = self.storage_usage(None)?;
        let (global_bytes, host_reserve_bytes) = self.budget_config_global()?;
        let mut vms = Vec::new();
        for vm in vm_ids {
            let config = self.budget_config(&vm)?;
            let usage = self.storage_usage(Some(&vm))?;
            let total = usage.total();
            vms.push(VmBudget {
                vm_id: vm,
                budget_bytes: config.vm_bytes,
                usage,
                total_bytes: total,
                exceeded: config.vm_bytes.is_some_and(|l| total > l),
                baselines: self.baseline_usage(&vm)?,
                runs: self.run_usage(&vm)?,
            });
        }
        vms.sort_by_key(|v| v.vm_id);
        let catalog_total = catalog_usage.total();
        Ok(BudgetManifest {
            format_version: crate::manifest::MANIFEST_FORMAT_VERSION,
            kind: "budget".into(),
            generated_at: now_rfc3339(),
            enforcement: "application_budget".into(),
            global_bytes,
            host_reserve_bytes,
            catalog_usage,
            catalog_total_bytes: catalog_total,
            catalog_exceeded: global_bytes.is_some_and(|l| catalog_total > l),
            vms,
        })
    }

    /// Writes the budget manifest to `budget.json` (atomic write) and records
    /// its SHA-256 in `catalog_meta`. Returns the manifest written.
    pub fn write_budget_manifest(&mut self) -> Result<BudgetManifest> {
        let m = self.budget_manifest()?;
        let rel = crate::paths::ManagedPath::parse(BUDGET_MANIFEST_FILE)?;
        let sha = crate::manifest::write_atomic(&self.layout, &rel, &m)?;
        self.with_tx(|tx| {
            tx.conn()
                .execute(
                    "INSERT INTO catalog_meta(key, value) VALUES ('budget_manifest_sha256', ?1)
                     ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                    params![sha],
                )
                .db()?;
            Ok(())
        })?;
        Ok(m)
    }

    fn budget_config_global(&self) -> Result<(Option<u64>, u64)> {
        let g: Option<(i64, i64)> = self
            .conn
            .query_row(
                "SELECT budget_bytes, host_reserve_bytes FROM storage_budgets WHERE scope_key = 'global'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .db()?;
        Ok((g.map(|g| u(g.0)), g.map(|g| u(g.1)).unwrap_or(0)))
    }

    fn baseline_usage(&self, vm: &VmId) -> Result<Vec<BaselineUsage>> {
        let mut st = self
            .conn
            .prepare(
                "SELECT b.baseline_id, d.allocated_bytes, coalesce(f.size_bytes, 0) + coalesce(t.size_bytes,0)
                 FROM baselines b JOIN disks d ON d.disk_id = b.base_disk_id
                 LEFT JOIN firmware_state f ON f.fw_id = b.vars_fw_id
                 LEFT JOIN baseline_tpm bt ON bt.baseline_id=b.baseline_id
                 LEFT JOIN firmware_state t ON t.fw_id=bt.fw_id
                 WHERE b.vm_id = ?1 ORDER BY b.baseline_id",
            )
            .db()?;
        let rows = st
            .query_map(params![vm.sql()], |r| {
                Ok((
                    idc::<BaselineId>(r, 0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, i64>(2)?,
                ))
            })
            .db()?;
        let mut out = Vec::new();
        for row in rows {
            let (baseline_id, d, f) = row.db()?;
            let (disk_bytes, vars_bytes) = (u(d), u(f));
            out.push(BaselineUsage {
                baseline_id,
                disk_bytes,
                vars_bytes,
                total_bytes: disk_bytes.saturating_add(vars_bytes),
            });
        }
        Ok(out)
    }

    fn run_usage(&self, vm: &VmId) -> Result<Vec<RunUsage>> {
        let mut st = self
            .conn
            .prepare(
                "SELECT r.run_id, r.ended_at IS NOT NULL,
                        coalesce((SELECT allocated_bytes FROM disks WHERE disk_id = r.overlay_disk_id), 0),
                        coalesce((SELECT size_bytes FROM firmware_state WHERE fw_id = r.vars_fw_id), 0),
                        coalesce((SELECT sum(size_bytes) FROM artifacts a
                                  WHERE a.run_id = r.run_id AND a.kind <> 'log'), 0),
                        coalesce((SELECT sum(size_bytes) FROM artifacts a
                                  WHERE a.run_id = r.run_id AND a.kind = 'log'), 0)
                 FROM runs r WHERE r.vm_id = ?1 ORDER BY r.run_id",
            )
            .db()?;
        let rows = st
            .query_map(params![vm.sql()], |r| {
                Ok((
                    idc::<RunId>(r, 0)?,
                    r.get::<_, bool>(1)?,
                    r.get::<_, i64>(2)?,
                    r.get::<_, i64>(3)?,
                    r.get::<_, i64>(4)?,
                    r.get::<_, i64>(5)?,
                ))
            })
            .db()?;
        let mut out = Vec::new();
        for row in rows {
            let (run_id, ended, o, f, a, l) = row.db()?;
            let (o, f, a, l) = (u(o), u(f), u(a), u(l));
            out.push(RunUsage {
                run_id,
                ended,
                overlay_bytes: o,
                firmware_bytes: f,
                artifact_bytes: a,
                log_bytes: l,
                total_bytes: o.saturating_add(f).saturating_add(a).saturating_add(l),
            });
        }
        Ok(out)
    }
}

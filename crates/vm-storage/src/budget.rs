//! Budget storage applicativo (ROADMAP 7.2 punto 8, sezione 8, ADR-0017).
//!
//! Natura del controllo: **budget applicativo** calcolato dai byte registrati
//! nel catalogo (dischi misurati, firmware, artefatti, log, media) più i file
//! di metadati, verificato prima di accettare una nuova esecuzione. Non è una
//! quota rigida del sistema operativo: tra un controllo e il successivo il
//! consumo può superare il budget, e nulla protegge dal riempimento del volume
//! causato da altri processi. Una policy che richiede quota rigida OS viene
//! rifiutata con [`StorageError::Unsupported`].

use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};

use crate::catalog::Catalog;
use crate::error::{Result, SqlResultExt, StorageError};
use crate::ids::{BaselineId, IdSql, RunId, VmId, idc};
use crate::time::now_rfc3339;

/// Natura del controllo richiesta dalla policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Enforcement {
    /// Budget applicativo con controllo prima dell'avvio e monitoraggio.
    ApplicationBudget,
    /// Quota rigida del sistema operativo: non implementata.
    OsHardQuota,
}

/// Consumo per categoria, in byte.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StorageUsage {
    /// Overlay delle esecuzioni (byte misurati).
    pub overlays: u64,
    /// Dischi base delle baseline e relative vars.
    pub baselines: u64,
    /// Dischi di lavoro e vars della VM.
    pub primary: u64,
    /// Vars delle esecuzioni.
    pub run_firmware: u64,
    /// Screenshot e report.
    pub artifacts: u64,
    /// Log.
    pub logs: u64,
    /// Registro media (solo nel totale del catalogo).
    pub media: u64,
    /// Database e file accessori (solo nel totale del catalogo).
    pub metadata: u64,
}

impl StorageUsage {
    /// Somma di tutte le categorie.
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
        ]
        .iter()
        .fold(0u64, |a, b| a.saturating_add(*b))
    }
}

/// Budget configurati.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BudgetConfig {
    /// Budget del catalogo (`None` = non dichiarato).
    pub global_bytes: Option<u64>,
    /// Riserva da lasciare libera sul volume host.
    pub host_reserve_bytes: u64,
    /// Budget della VM (`None` = non dichiarato).
    pub vm_bytes: Option<u64>,
}

/// Esito positivo del controllo, con i valori usati.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BudgetCheck {
    /// Consumo della VM.
    pub vm_usage: StorageUsage,
    /// Consumo del catalogo.
    pub catalog_usage: StorageUsage,
    /// Budget applicati.
    pub config: BudgetConfig,
    /// Stima richiesta per la nuova esecuzione.
    pub requested_bytes: u64,
}

fn u(v: i64) -> u64 {
    u64::try_from(v).unwrap_or(0)
}

fn i(v: u64) -> Result<i64> {
    i64::try_from(v).map_err(|_| StorageError::Constraint("valore fuori intervallo".into()))
}

impl Catalog {
    /// Imposta budget del catalogo e riserva host.
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

    /// Imposta il budget di una VM.
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

    /// Budget applicabili a una VM.
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

    /// Consumo di una VM (`Some`) o dell'intero catalogo (`None`, include media
    /// e metadati).
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
        };
        if vm.is_none() {
            let media: i64 = self
                .conn
                .query_row("SELECT coalesce(sum(size_bytes),0) FROM media", [], |r| {
                    r.get(0)
                })
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

    /// Aggiorna `allocated_bytes` dei dischi misurando i file (dimensione del
    /// file; per file sparsi è la dimensione apparente, quindi una stima per eccesso).
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

    /// Controlla se una nuova esecuzione con crescita stimata `requested_bytes`
    /// rientra nei budget dichiarati e lascia libera la riserva host.
    /// `host_free_bytes` è lo spazio libero del volume misurato dal chiamante.
    pub fn check_new_run(
        &self,
        vm: &VmId,
        requested_bytes: u64,
        host_free_bytes: u64,
        enforcement: Enforcement,
    ) -> Result<BudgetCheck> {
        if enforcement == Enforcement::OsHardQuota {
            return Err(StorageError::Unsupported(
                "quota rigida del sistema operativo non implementata",
            ));
        }
        let config = self.budget_config(vm)?;
        let vm_usage = self.storage_usage(Some(vm))?;
        let catalog_usage = self.storage_usage(None)?;
        if let Some(limit) = config.vm_bytes {
            let need = vm_usage.total().saturating_add(requested_bytes);
            if need > limit {
                return Err(StorageError::ResourceLimit(format!(
                    "budget della VM superato: {need} > {limit} byte"
                )));
            }
        }
        if let Some(limit) = config.global_bytes {
            let need = catalog_usage.total().saturating_add(requested_bytes);
            if need > limit {
                return Err(StorageError::ResourceLimit(format!(
                    "budget del catalogo superato: {need} > {limit} byte"
                )));
            }
        }
        let need_free = requested_bytes.saturating_add(config.host_reserve_bytes);
        if host_free_bytes < need_free {
            return Err(StorageError::ResourceLimit(format!(
                "riserva host non rispettata: liberi {host_free_bytes}, richiesti {need_free} byte"
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

/// Nome del manifest di budget nella radice del catalogo.
pub const BUDGET_MANIFEST_FILE: &str = "budget.json";

/// Occupazione di una baseline (disco base e vars).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BaselineUsage {
    /// Baseline.
    pub baseline_id: BaselineId,
    /// Byte del disco base.
    pub disk_bytes: u64,
    /// Byte del vars della baseline.
    pub vars_bytes: u64,
    /// Totale.
    pub total_bytes: u64,
}

/// Occupazione di un'esecuzione.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunUsage {
    /// Esecuzione.
    pub run_id: RunId,
    /// Esecuzione conclusa (overlay conservato o in attesa di eliminazione).
    pub ended: bool,
    /// Byte dell'overlay.
    pub overlay_bytes: u64,
    /// Byte del vars dell'esecuzione.
    pub firmware_bytes: u64,
    /// Screenshot e report.
    pub artifact_bytes: u64,
    /// Log.
    pub log_bytes: u64,
    /// Totale.
    pub total_bytes: u64,
}

/// Occupazione e limite di una VM.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VmBudget {
    /// VM.
    pub vm_id: VmId,
    /// Budget dichiarato (`None` = non dichiarato).
    pub budget_bytes: Option<u64>,
    /// Consumo per categoria.
    pub usage: StorageUsage,
    /// Totale.
    pub total_bytes: u64,
    /// Budget dichiarato e superato.
    pub exceeded: bool,
    /// Baseline della VM.
    pub baselines: Vec<BaselineUsage>,
    /// Esecuzioni della VM.
    pub runs: Vec<RunUsage>,
}

/// Manifest di budget: occupazione per catalogo, VM, baseline ed esecuzione,
/// con i limiti dichiarati. Fotografia dei byte registrati nel catalogo (vedi
/// [`Catalog::refresh_disk_sizes`]); non è una quota rigida del sistema.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BudgetManifest {
    /// Versione del formato.
    pub format_version: u32,
    /// Sempre `"budget"`.
    pub kind: String,
    /// Istante di produzione (UTC).
    pub generated_at: String,
    /// Natura del controllo: sempre `"application_budget"`.
    pub enforcement: String,
    /// Budget del catalogo.
    pub global_bytes: Option<u64>,
    /// Riserva da lasciare libera sul volume host.
    pub host_reserve_bytes: u64,
    /// Consumo del catalogo (media e metadati inclusi).
    pub catalog_usage: StorageUsage,
    /// Totale del catalogo.
    pub catalog_total_bytes: u64,
    /// Budget del catalogo dichiarato e superato.
    pub catalog_exceeded: bool,
    /// VM, ordinate per identificativo.
    pub vms: Vec<VmBudget>,
}

/// Richiesta di controllo del budget per una nuova esecuzione.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RunBudgetRequest {
    /// Crescita stimata dell'esecuzione (overlay, vars, artefatti, log).
    pub requested_bytes: u64,
    /// Spazio libero del volume misurato dal chiamante.
    pub host_free_bytes: u64,
    /// Natura del controllo richiesta dalla policy.
    pub enforcement: Enforcement,
}

impl Catalog {
    /// Costruisce il manifest di budget dai byte registrati.
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

    /// Scrive il manifest di budget in `budget.json` (scrittura atomica) e ne
    /// registra lo SHA-256 in `catalog_meta`. Restituisce il manifest scritto.
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
                "SELECT b.baseline_id, d.allocated_bytes, coalesce(f.size_bytes, 0)
                 FROM baselines b JOIN disks d ON d.disk_id = b.base_disk_id
                 LEFT JOIN firmware_state f ON f.fw_id = b.vars_fw_id
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

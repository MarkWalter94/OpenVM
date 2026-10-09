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

use crate::catalog::Catalog;
use crate::compat::{IdSql, VmId};
use crate::error::{Result, SqlResultExt, StorageError};
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
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
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

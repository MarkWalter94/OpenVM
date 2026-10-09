//! Operazioni composte che attraversano DB e filesystem tramite il journal.
//!
//! I file dei dischi sono prodotti dal chiamante (in prodotto: `qemu-backend`
//! con `qemu-img`) dentro il percorso temporaneo indicato; lo storage gestisce
//! journal, spostamenti, hash, righe del DB e manifest.

use std::path::Path;

use crate::budget::{BudgetCheck, RunBudgetRequest};
use crate::catalog::{Catalog, Intent, IntentSpec};
use crate::enums::{
    DiskRole, DiskStatus, FirmwareScope, IntentKind, Lifecycle, RunState, TargetKind,
};
use crate::error::{Result, StorageError};
use crate::ids::{BaselineId, DiskId, FirmwareId, OperationId, RunId, VmId};
use crate::layout::CatalogLayout;
use crate::manifest::{self, BaselineManifest, MANIFEST_FORMAT_VERSION, ManifestFile};
use crate::records::{BaselineRecord, DiskRecord, FirmwareRecord, RunRecord};
use crate::time::now_rfc3339;

/// Richiesta di promozione di una baseline a VM spenta.
#[derive(Debug, Clone)]
pub struct PromoteBaseline {
    /// VM (deve risultare `stopped`).
    pub vm_id: VmId,
    /// Etichetta.
    pub label: String,
    /// Arresto pulito osservato prima della promozione.
    pub clean_shutdown: bool,
    /// Baseline protetta (non cancellabile).
    pub protected: bool,
    /// Dimensione virtuale del disco base.
    pub virtual_size_bytes: u64,
}

type Producer<'a> = Box<dyn FnOnce(&Path) -> Result<()> + 'a>;

impl Catalog {
    /// Materializza una nuova baseline: il chiamante scrive il disco base
    /// autonomo (e, se presenti, le vars) nei temporanei; lo storage li rende di
    /// sola lettura, li sposta in `vms/<vm>/base/<baseline>/`, scrive il
    /// manifest e registra disco, firmware e baseline in un'unica transazione.
    pub fn promote_baseline(
        &mut self,
        req: &PromoteBaseline,
        write_disk: impl FnOnce(&Path) -> Result<()>,
        write_vars: Option<Producer<'_>>,
    ) -> Result<BaselineId> {
        let vm = self.read(|tx| tx.vm(&req.vm_id))?;
        if vm.lifecycle != Lifecycle::Stopped {
            return Err(StorageError::StateConflict(format!(
                "baseline richiesta con VM in stato {}",
                vm.lifecycle
            )));
        }
        let op = OperationId::new_v7();
        let baseline_id = BaselineId::new_v7();
        let disk_id = DiskId::new_v7();
        let fw_id = FirmwareId::new_v7();
        let mut intents: Vec<Intent> = Vec::new();
        let result = (|| -> Result<BaselineId> {
            let di = self.begin_intent(IntentSpec {
                kind: IntentKind::PromoteBaseline,
                vm_id: Some(req.vm_id),
                operation_id: Some(op),
                target_kind: TargetKind::Disk,
                target_id: Some(disk_id.to_string()),
                final_path: Some(CatalogLayout::baseline_disk(&req.vm_id, &baseline_id)),
                read_only: true,
            })?;
            intents.push(di.clone());
            write_disk(&di.temp_path)?;
            self.mark_applied(&di)?;
            let vi = match write_vars {
                Some(w) => {
                    let vi = self.begin_intent(IntentSpec {
                        kind: IntentKind::CopyVars,
                        vm_id: Some(req.vm_id),
                        operation_id: Some(op),
                        target_kind: TargetKind::Firmware,
                        target_id: Some(fw_id.to_string()),
                        final_path: Some(CatalogLayout::baseline_vars(&req.vm_id, &baseline_id)),
                        read_only: true,
                    })?;
                    intents.push(vi.clone());
                    w(&vi.temp_path)?;
                    self.mark_applied(&vi)?;
                    Some(vi)
                }
                None => None,
            };
            let disk_rel = CatalogLayout::baseline_disk(&req.vm_id, &baseline_id);
            let disk_abs = self.layout.resolve(&disk_rel)?;
            let disk_sha = manifest::sha256_file(&disk_abs)?;
            let disk_len = std::fs::metadata(&disk_abs)
                .map_err(|e| StorageError::io(&disk_abs, e))?
                .len();
            let vars_info = match &vi {
                Some(_) => {
                    let rel = CatalogLayout::baseline_vars(&req.vm_id, &baseline_id);
                    let abs = self.layout.resolve(&rel)?;
                    let len = std::fs::metadata(&abs)
                        .map_err(|e| StorageError::io(&abs, e))?
                        .len();
                    Some((rel, manifest::sha256_file(&abs)?, len))
                }
                None => None,
            };
            let created_at = now_rfc3339();
            let m = BaselineManifest {
                format_version: MANIFEST_FORMAT_VERSION,
                kind: "baseline".into(),
                baseline_id,
                vm_id: req.vm_id,
                label: req.label.clone(),
                config_revision: vm.revision,
                config: vm.config.clone(),
                disk: ManifestFile {
                    role: "base".into(),
                    path: disk_rel.to_string(),
                    sha256: Some(disk_sha.clone()),
                },
                vars: vars_info.as_ref().map(|(rel, sha, _)| ManifestFile {
                    role: "vars".into(),
                    path: rel.to_string(),
                    sha256: Some(sha.clone()),
                }),
                created_powered_off: true,
                clean_shutdown: req.clean_shutdown,
                created_at: created_at.clone(),
            };
            let manifest_sha = manifest::write_atomic(
                &self.layout,
                &CatalogLayout::baseline_manifest(&req.vm_id, &baseline_id),
                &m,
            )?;
            self.commit_many(&intents, |tx| {
                tx.insert_disk(&DiskRecord {
                    disk_id,
                    vm_id: req.vm_id,
                    role: DiskRole::Base,
                    backing_disk_id: None,
                    virtual_size_bytes: req.virtual_size_bytes,
                    allocated_bytes: disk_len,
                    managed_path: disk_rel.clone(),
                    sha256: Some(disk_sha.clone()),
                    status: DiskStatus::Ready,
                    created_at: created_at.clone(),
                })?;
                if let Some((rel, sha, len)) = &vars_info {
                    tx.insert_firmware(&FirmwareRecord {
                        fw_id,
                        vm_id: req.vm_id,
                        scope: FirmwareScope::Baseline,
                        source_fw_id: None,
                        managed_path: rel.clone(),
                        sha256: Some(sha.clone()),
                        size_bytes: *len,
                        created_at: created_at.clone(),
                    })?;
                }
                tx.insert_baseline(&BaselineRecord {
                    baseline_id,
                    vm_id: req.vm_id,
                    base_disk_id: disk_id,
                    vars_fw_id: vars_info.as_ref().map(|_| fw_id),
                    label: req.label.clone(),
                    manifest_sha256: manifest_sha.clone(),
                    clean_shutdown: req.clean_shutdown,
                    protected: req.protected,
                    created_at: created_at.clone(),
                })
            })?;
            Ok(baseline_id)
        })();
        match result {
            Ok(id) => {
                self.write_vm_manifest(&req.vm_id)?;
                Ok(id)
            }
            Err(e) => {
                self.abort_all(&intents);
                Err(e)
            }
        }
    }

    /// Prepara un'esecuzione da una baseline (avvio o ripristino): il chiamante
    /// crea l'overlay nel temporaneo (riceve anche il percorso assoluto della
    /// base); le vars della baseline sono copiate dallo storage. Overlay, vars
    /// ed esecuzione sono registrati in un'unica transazione.
    ///
    /// Nota per `qemu-backend`: il riferimento al backing scritto nell'overlay
    /// deve restare valido dopo lo spostamento da `tmp/` a
    /// `vms/<vm>/overlays/` (percorso assoluto validato o relativo calcolato
    /// rispetto alla destinazione finale).
    pub fn prepare_run_from_baseline(
        &mut self,
        vm_id: &VmId,
        baseline_id: &BaselineId,
        write_overlay: impl FnOnce(&Path, &Path) -> Result<()>,
    ) -> Result<RunRecord> {
        let (vm, baseline, base_disk, base_vars) = self.read(|tx| {
            let vm = tx.vm(vm_id)?;
            let b = tx.baseline(baseline_id)?;
            let d = tx.disk(&b.base_disk_id)?;
            let f = b.vars_fw_id.as_ref().map(|f| tx.firmware(f)).transpose()?;
            Ok((vm, b, d, f))
        })?;
        if vm.lifecycle != Lifecycle::Stopped {
            return Err(StorageError::StateConflict(format!(
                "esecuzione richiesta con VM in stato {}",
                vm.lifecycle
            )));
        }
        if baseline.vm_id != *vm_id || base_disk.status != DiskStatus::Ready {
            return Err(StorageError::StateConflict(
                "baseline non utilizzabile per questa VM".into(),
            ));
        }
        // Rilevatore di scrittori e protezione della baseline per tutta la
        // preparazione (ADR-0025 punto 2): base e vars aperti con il solo
        // `FILE_SHARE_READ`. La verifica completa dell'hash avviene in
        // `guard_run`, prima dell'avvio.
        let _baseline_guard = self.baseline_guard(baseline_id, false)?;
        let op = OperationId::new_v7();
        let run_id = RunId::new_v7();
        let overlay_id = DiskId::new_v7();
        let fw_id = FirmwareId::new_v7();
        let mut intents: Vec<Intent> = Vec::new();
        let result = (|| -> Result<RunRecord> {
            let base_abs = self.layout.resolve(&base_disk.managed_path)?;
            let oi = self.begin_intent(IntentSpec {
                kind: IntentKind::CreateOverlay,
                vm_id: Some(*vm_id),
                operation_id: Some(op),
                target_kind: TargetKind::Disk,
                target_id: Some(overlay_id.to_string()),
                final_path: Some(CatalogLayout::overlay(vm_id, &run_id)),
                read_only: false,
            })?;
            intents.push(oi.clone());
            write_overlay(&oi.temp_path, &base_abs)?;
            self.mark_applied(&oi)?;
            let vars = match &base_vars {
                Some(src) => {
                    let vi = self.begin_intent(IntentSpec {
                        kind: IntentKind::CopyVars,
                        vm_id: Some(*vm_id),
                        operation_id: Some(op),
                        target_kind: TargetKind::Firmware,
                        target_id: Some(fw_id.to_string()),
                        final_path: Some(CatalogLayout::run_vars(vm_id, &run_id)),
                        read_only: false,
                    })?;
                    intents.push(vi.clone());
                    let src_abs = self.layout.resolve(&src.managed_path)?;
                    copy_writable(&src_abs, &vi.temp_path)?;
                    self.mark_applied(&vi)?;
                    let len = std::fs::metadata(vi.final_path.as_deref().unwrap_or(&src_abs))
                        .map_err(|e| StorageError::io(&src_abs, e))?
                        .len();
                    Some((src.fw_id, src.sha256.clone(), len))
                }
                None => None,
            };
            let overlay_rel = CatalogLayout::overlay(vm_id, &run_id);
            let overlay_abs = self.layout.resolve(&overlay_rel)?;
            let overlay_len = std::fs::metadata(&overlay_abs)
                .map_err(|e| StorageError::io(&overlay_abs, e))?
                .len();
            let now = now_rfc3339();
            let run = RunRecord {
                run_id,
                vm_id: *vm_id,
                baseline_id: Some(*baseline_id),
                overlay_disk_id: Some(overlay_id),
                vars_fw_id: vars.as_ref().map(|_| fw_id),
                state: RunState::Starting,
                started_at: now.clone(),
                ended_at: None,
                termination_cause: None,
            };
            self.commit_many(&intents, |tx| {
                tx.insert_disk(&DiskRecord {
                    disk_id: overlay_id,
                    vm_id: *vm_id,
                    role: DiskRole::Overlay,
                    backing_disk_id: Some(base_disk.disk_id),
                    virtual_size_bytes: base_disk.virtual_size_bytes,
                    allocated_bytes: overlay_len,
                    managed_path: overlay_rel.clone(),
                    sha256: None,
                    status: DiskStatus::Ready,
                    created_at: now.clone(),
                })?;
                if let Some((src_id, sha, len)) = &vars {
                    tx.insert_firmware(&FirmwareRecord {
                        fw_id,
                        vm_id: *vm_id,
                        scope: FirmwareScope::Run,
                        source_fw_id: Some(*src_id),
                        managed_path: CatalogLayout::run_vars(vm_id, &run_id),
                        sha256: sha.clone(),
                        size_bytes: *len,
                        created_at: now.clone(),
                    })?;
                }
                tx.insert_run(&run)
            })?;
            Ok(run)
        })();
        match result {
            Ok(run) => {
                self.layout
                    .ensure_dir(&CatalogLayout::run_artifacts(&run.run_id))?;
                self.layout
                    .ensure_dir(&CatalogLayout::run_logs(&run.run_id))?;
                Ok(run)
            }
            Err(e) => {
                self.abort_all(&intents);
                Err(e)
            }
        }
    }

    /// Come [`Self::prepare_run_from_baseline`], preceduta dal controllo del
    /// budget ([`Self::check_new_run`]): se il budget della VM o del catalogo o
    /// la riserva host non lo consentono, l'esecuzione è rifiutata con
    /// [`StorageError::ResourceLimit`] prima di qualunque intento o file.
    pub fn prepare_run_within_budget(
        &mut self,
        vm_id: &VmId,
        baseline_id: &BaselineId,
        budget: &RunBudgetRequest,
        write_overlay: impl FnOnce(&Path, &Path) -> Result<()>,
    ) -> Result<(RunRecord, BudgetCheck)> {
        let check = self.check_new_run(
            vm_id,
            budget.requested_bytes,
            budget.host_free_bytes,
            budget.enforcement,
        )?;
        let run = self.prepare_run_from_baseline(vm_id, baseline_id, write_overlay)?;
        Ok((run, check))
    }

    /// Annulla, in ordine inverso, gli intenti ancora aperti. Gli errori sono
    /// ignorati qui: un intento non annullato resta aperto e sarà risolto da
    /// [`Catalog::recover`] al prossimo avvio.
    fn abort_all(&mut self, intents: &[Intent]) {
        for i in intents.iter().rev() {
            let _ = self.abort(i);
        }
    }
}

#[allow(clippy::permissions_set_readonly_false)]
fn copy_writable(src: &Path, dst: &Path) -> Result<()> {
    std::fs::copy(src, dst).map_err(|e| StorageError::io(dst, e))?;
    // `fs::copy` preserva l'attributo di sola lettura della sorgente (vars
    // della baseline): la copia di un'esecuzione deve essere scrivibile.
    let mut p = std::fs::metadata(dst)
        .map_err(|e| StorageError::io(dst, e))?
        .permissions();
    if p.readonly() {
        p.set_readonly(false);
        std::fs::set_permissions(dst, p).map_err(|e| StorageError::io(dst, e))?;
    }
    Ok(())
}

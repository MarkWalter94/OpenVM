//! Journal a intenti: crash simulato in ogni punto e riconciliazione.
//!
//! "Crash" = il `Catalog` viene rilasciato senza completare l'operazione (come
//! un processo ucciso: nessun codice di pulizia viene eseguito) e il catalogo
//! viene riaperto, cosa che esegue `recover()`.

mod common;

use std::path::Path;

use vm_storage::compat::{BaselineId, DiskId, OperationId, RunId, VmId};
use vm_storage::{
    Catalog, CatalogLayout, DiskRecord, DiskRole, DiskStatus, Intent, IntentKind, IntentSpec,
    IntentState, IntegrityIssue, Lifecycle, RecoveryOutcome, RecoveryReport, RunRecord, RunState,
    StorageError, TargetKind, now_rfc3339,
};

struct Fixture {
    dir: tempfile::TempDir,
    vm: VmId,
    baseline: BaselineId,
    base_disk: DiskId,
    base_hash: String,
}

fn fixture() -> (Fixture, Catalog) {
    let dir = common::tempdir();
    let mut cat = Catalog::open(dir.path()).unwrap();
    let vm = common::new_vm(&mut cat, "vm");
    let baseline = common::baseline(&mut cat, &vm, true);
    let base_disk = cat
        .read(|tx| Ok(tx.baseline(&baseline)?.base_disk_id))
        .unwrap();
    let base_hash = common::base_hash(&mut cat, &baseline);
    (
        Fixture {
            dir,
            vm,
            baseline,
            base_disk,
            base_hash,
        },
        cat,
    )
}

impl Fixture {
    fn crash_and_reopen(&self, cat: Catalog) -> (Catalog, RecoveryReport) {
        drop(cat);
        let cat = Catalog::open(self.dir.path()).unwrap();
        let rep = cat.open_report().unwrap().recovery.clone();
        (cat, rep)
    }

    /// Proprietà comuni a ogni esito della riconciliazione (7.4, riga "Crash").
    fn assert_safe(&self, cat: &mut Catalog) {
        assert!(cat.open_intents().unwrap().is_empty(), "nessun intento aperto");
        assert_eq!(
            common::base_hash(cat, &self.baseline),
            self.base_hash,
            "hash della base invariato"
        );
        let b = cat.read(|tx| tx.baseline(&self.baseline)).unwrap();
        assert_eq!(b.base_disk_id, self.base_disk, "baseline intatta nel DB");
        assert!(cat.verify_baseline_manifest(&self.baseline, true).unwrap().is_empty());
        let tmp_entries = std::fs::read_dir(self.dir.path().join("tmp")).unwrap().count();
        assert_eq!(tmp_entries, 0, "tmp/ vuota");
    }

    fn overlay_intent(&self, cat: &mut Catalog) -> (Intent, DiskId, RunId) {
        let run = RunId::new_v7();
        let disk = DiskId::new_v7();
        let i = cat
            .begin_intent(IntentSpec {
                kind: IntentKind::CreateOverlay,
                vm_id: Some(self.vm),
                operation_id: Some(OperationId::new_v7()),
                target_kind: TargetKind::Disk,
                target_id: Some(disk.to_string()),
                final_path: Some(CatalogLayout::overlay(&self.vm, &run)),
                read_only: false,
            })
            .unwrap();
        (i, disk, run)
    }

    fn overlay_row(&self, disk: DiskId, run: &RunId) -> DiskRecord {
        DiskRecord {
            disk_id: disk,
            vm_id: self.vm,
            role: DiskRole::Overlay,
            backing_disk_id: Some(self.base_disk),
            virtual_size_bytes: 1024,
            allocated_bytes: 0,
            managed_path: CatalogLayout::overlay(&self.vm, run),
            sha256: None,
            status: DiskStatus::Ready,
            created_at: now_rfc3339(),
        }
    }
}

fn single(rep: &RecoveryReport) -> (RecoveryOutcome, &str) {
    assert_eq!(rep.intents.len(), 1, "{rep:?}");
    (rep.intents[0].outcome, rep.intents[0].detail.as_str())
}

fn write(p: &Path) {
    common::write_bytes(p, b"QFI\xfb overlay").unwrap();
}

// ------------------------------------------------------------ creazione

#[test]
fn crash_after_begin_nothing_to_undo() {
    let (fx, mut cat) = fixture();
    let (i, _, _) = fx.overlay_intent(&mut cat);
    let (mut cat, rep) = fx.crash_and_reopen(cat);
    assert_eq!(single(&rep), (RecoveryOutcome::RolledBack, "nothing_to_undo"));
    assert_eq!(cat.intent_state(i.id).unwrap(), IntentState::Aborted);
    fx.assert_safe(&mut cat);
}

#[test]
fn crash_during_creation_removes_temp_file() {
    let (fx, mut cat) = fixture();
    let (i, _, _) = fx.overlay_intent(&mut cat);
    write(&i.temp_path);
    let (mut cat, rep) = fx.crash_and_reopen(cat);
    assert_eq!(single(&rep), (RecoveryOutcome::RolledBack, "temp_removed"));
    assert!(!i.temp_path.exists());
    fx.assert_safe(&mut cat);
}

#[test]
fn crash_between_rename_and_journal_update_removes_orphan() {
    let (fx, mut cat) = fixture();
    let (i, _, _) = fx.overlay_intent(&mut cat);
    write(&i.temp_path);
    // Rinomina avvenuta, aggiornamento del journal (applied) non registrato.
    std::fs::rename(&i.temp_path, i.final_path.as_ref().unwrap()).unwrap();
    let (mut cat, rep) = fx.crash_and_reopen(cat);
    assert_eq!(
        single(&rep),
        (RecoveryOutcome::RolledBack, "orphan_final_removed")
    );
    assert!(!i.final_path.as_ref().unwrap().exists());
    fx.assert_safe(&mut cat);
}

/// 7.4 "Crash", primo caso: file presente e DB assente → il file viene rimosso.
#[test]
fn crash_after_apply_file_present_db_absent() {
    let (fx, mut cat) = fixture();
    let (i, disk, _) = fx.overlay_intent(&mut cat);
    write(&i.temp_path);
    cat.mark_applied(&i).unwrap();
    assert!(i.final_path.as_ref().unwrap().is_file());
    let (mut cat, rep) = fx.crash_and_reopen(cat);
    assert_eq!(
        single(&rep),
        (RecoveryOutcome::RolledBack, "orphan_final_removed")
    );
    assert_eq!(rep.intents[0].prior_state, IntentState::Applied);
    assert!(!i.final_path.as_ref().unwrap().exists());
    assert!(cat.read(|tx| tx.disk(&disk)).is_err());
    fx.assert_safe(&mut cat);
}

/// Aggiornamento del DB fallito a metà (transazione annullata): l'intento
/// resta `applied` e la riconciliazione lo annulla.
#[test]
fn failed_db_update_is_rolled_back() {
    let (fx, mut cat) = fixture();
    let (i, disk, run) = fx.overlay_intent(&mut cat);
    write(&i.temp_path);
    cat.mark_applied(&i).unwrap();
    let row = fx.overlay_row(disk, &run);
    let err = cat
        .commit(&i, |tx| {
            tx.insert_disk(&row)?;
            Err(StorageError::Constraint("interruzione simulata".into()))
        })
        .unwrap_err();
    assert!(matches!(err, StorageError::Constraint(_)));
    assert_eq!(cat.intent_state(i.id).unwrap(), IntentState::Applied);
    assert!(cat.read(|tx| tx.disk(&disk)).is_err(), "transazione annullata");
    let (mut cat, rep) = fx.crash_and_reopen(cat);
    assert_eq!(single(&rep).0, RecoveryOutcome::RolledBack);
    fx.assert_safe(&mut cat);
}

/// Un `commit` la cui closure non registra la riga di destinazione è rifiutato.
#[test]
fn commit_without_target_row_is_refused() {
    let (fx, mut cat) = fixture();
    let (i, _, _) = fx.overlay_intent(&mut cat);
    write(&i.temp_path);
    cat.mark_applied(&i).unwrap();
    assert!(matches!(
        cat.commit(&i, |_| Ok(())),
        Err(StorageError::StateConflict(_))
    ));
    cat.abort(&i).unwrap();
    fx.assert_safe(&mut cat);
}

#[test]
fn crash_after_commit_keeps_everything() {
    let (fx, mut cat) = fixture();
    let (i, disk, run) = fx.overlay_intent(&mut cat);
    write(&i.temp_path);
    cat.mark_applied(&i).unwrap();
    let row = fx.overlay_row(disk, &run);
    cat.commit(&i, |tx| tx.insert_disk(&row)).unwrap();
    let (mut cat, rep) = fx.crash_and_reopen(cat);
    assert!(rep.intents.is_empty());
    assert!(rep.integrity.is_empty(), "{:?}", rep.integrity);
    assert!(i.final_path.as_ref().unwrap().is_file());
    assert_eq!(
        cat.read(|tx| tx.disk(&disk)).unwrap().status,
        DiskStatus::Ready
    );
    assert_eq!(cat.intent_state(i.id).unwrap(), IntentState::Committed);
    fx.assert_safe(&mut cat);
}

/// Riga e file entrambi presenti con intento non concluso: completamento.
#[test]
fn row_and_file_present_is_rolled_forward() {
    let (fx, mut cat) = fixture();
    let (i, disk, run) = fx.overlay_intent(&mut cat);
    write(&i.temp_path);
    cat.mark_applied(&i).unwrap();
    let row = fx.overlay_row(disk, &run);
    cat.with_tx(|tx| tx.insert_disk(&row)).unwrap();
    let (mut cat, rep) = fx.crash_and_reopen(cat);
    assert_eq!(single(&rep), (RecoveryOutcome::RolledForward, "rolled_forward"));
    assert_eq!(cat.intent_state(i.id).unwrap(), IntentState::Committed);
    fx.assert_safe(&mut cat);
}

/// 7.4 "Crash", secondo caso: DB presente e file assente → VM `recovering`,
/// overlay `missing`, esecuzione `failed`; baseline non toccata.
#[test]
fn db_present_file_absent_marks_recovering_and_keeps_baseline() {
    let (fx, mut cat) = fixture();
    let (i, disk, run) = fx.overlay_intent(&mut cat);
    write(&i.temp_path);
    cat.mark_applied(&i).unwrap();
    let row = fx.overlay_row(disk, &run);
    let run_rec = RunRecord {
        run_id: run,
        vm_id: fx.vm,
        baseline_id: Some(fx.baseline),
        overlay_disk_id: Some(disk),
        vars_fw_id: None,
        state: RunState::Starting,
        started_at: now_rfc3339(),
        ended_at: None,
        termination_cause: None,
    };
    cat.with_tx(|tx| {
        tx.insert_disk(&row)?;
        tx.insert_run(&run_rec)
    })
    .unwrap();
    std::fs::remove_file(i.final_path.as_ref().unwrap()).unwrap();
    let (mut cat, rep) = fx.crash_and_reopen(cat);
    assert_eq!(
        single(&rep),
        (RecoveryOutcome::MarkedRecovering, "db_without_file")
    );
    let vm = cat.read(|tx| tx.vm(&fx.vm)).unwrap();
    assert_eq!(vm.lifecycle, Lifecycle::Recovering);
    assert_eq!(
        cat.read(|tx| tx.disk(&disk)).unwrap().status,
        DiskStatus::Missing
    );
    let r = cat.read(|tx| tx.run(&run)).unwrap();
    assert_eq!(r.state, RunState::Failed);
    assert!(r.ended_at.is_some());
    fx.assert_safe(&mut cat);
}

/// Stesso caso fuori dal journal (file di un overlay registrato sparito):
/// rilevato dalla verifica di integrità all'apertura.
#[test]
fn integrity_scan_detects_missing_overlay() {
    let (fx, mut cat) = fixture();
    let run = common::run_from(&mut cat, &fx.vm, &fx.baseline);
    let overlay = cat
        .read(|tx| tx.disk(&run.overlay_disk_id.unwrap()))
        .unwrap();
    std::fs::remove_file(cat.layout().resolve(&overlay.managed_path).unwrap()).unwrap();
    let (mut cat, rep) = fx.crash_and_reopen(cat);
    assert!(rep.intents.is_empty());
    assert!(matches!(
        rep.integrity.as_slice(),
        [IntegrityIssue::DiskMissing { .. }]
    ));
    assert_eq!(
        cat.read(|tx| tx.run(&run.run_id)).unwrap().state,
        RunState::Failed
    );
    fx.assert_safe(&mut cat);
}

/// Base mancante: solo segnalata, nessuna modifica a righe di baseline.
#[test]
#[allow(clippy::permissions_set_readonly_false)]
fn missing_base_is_reported_not_modified() {
    let (fx, cat) = fixture();
    let rel = CatalogLayout::baseline_disk(&fx.vm, &fx.baseline);
    let p = cat.layout().resolve(&rel).unwrap();
    let mut perm = std::fs::metadata(&p).unwrap().permissions();
    assert!(perm.readonly(), "base resa di sola lettura");
    perm.set_readonly(false);
    std::fs::set_permissions(&p, perm).unwrap();
    std::fs::remove_file(&p).unwrap();
    let (mut cat, rep) = fx.crash_and_reopen(cat);
    assert!(matches!(
        rep.integrity.as_slice(),
        [IntegrityIssue::BaseDiskMissing { .. }]
    ));
    let b = cat.read(|tx| tx.baseline(&fx.baseline)).unwrap();
    assert_eq!(b.base_disk_id, fx.base_disk);
    assert_eq!(
        cat.read(|tx| tx.disk(&fx.base_disk)).unwrap().status,
        DiskStatus::Ready,
        "riga della base non modificata"
    );
}

// -------------------------------------------------- ripristino (overlay + vars)

#[test]
fn crash_during_restore_after_vars_copy() {
    let (fx, mut cat) = fixture();
    // Prima esecuzione regolare, poi "ripristino" interrotto.
    common::run_from(&mut cat, &fx.vm, &fx.baseline);
    let op = OperationId::new_v7();
    let run = RunId::new_v7();
    let mk = |kind, tk, final_path| IntentSpec {
        kind,
        vm_id: Some(fx.vm),
        operation_id: Some(op),
        target_kind: tk,
        target_id: Some(DiskId::new_v7().to_string()),
        final_path: Some(final_path),
        read_only: false,
    };
    let oi = cat
        .begin_intent(mk(
            IntentKind::CreateOverlay,
            TargetKind::Disk,
            CatalogLayout::overlay(&fx.vm, &run),
        ))
        .unwrap();
    write(&oi.temp_path);
    cat.mark_applied(&oi).unwrap();
    let vi = cat
        .begin_intent(mk(
            IntentKind::CopyVars,
            TargetKind::Firmware,
            CatalogLayout::run_vars(&fx.vm, &run),
        ))
        .unwrap();
    common::write_bytes(&vi.temp_path, b"vars").unwrap();
    let (mut cat, rep) = fx.crash_and_reopen(cat);
    assert_eq!(rep.intents.len(), 2);
    assert!(rep
        .intents
        .iter()
        .all(|r| r.outcome == RecoveryOutcome::RolledBack));
    assert!(!oi.final_path.as_ref().unwrap().exists());
    fx.assert_safe(&mut cat);
    // Dopo la riconciliazione un nuovo ripristino riesce.
    common::run_from(&mut cat, &fx.vm, &fx.baseline);
}

#[test]
fn concurrent_operations_on_same_vm_are_refused() {
    let (fx, mut cat) = fixture();
    let (_i, _, _) = fx.overlay_intent(&mut cat);
    let err = cat
        .prepare_run_from_baseline(&fx.vm, &fx.baseline, |t, _| {
            common::write_bytes(t, b"x")
        })
        .unwrap_err();
    assert!(matches!(err, StorageError::VmBusy(_)), "{err:?}");
    assert_eq!(err.public_code(), Some("vm_busy"));
}

// ------------------------------------------------------- promozione baseline

#[test]
fn crash_during_baseline_promotion_removes_partial_baseline() {
    let (fx, mut cat) = fixture();
    let new_b = BaselineId::new_v7();
    let i = cat
        .begin_intent(IntentSpec {
            kind: IntentKind::PromoteBaseline,
            vm_id: Some(fx.vm),
            operation_id: None,
            target_kind: TargetKind::Disk,
            target_id: Some(DiskId::new_v7().to_string()),
            final_path: Some(CatalogLayout::baseline_disk(&fx.vm, &new_b)),
            read_only: true,
        })
        .unwrap();
    common::write_bytes(&i.temp_path, b"nuova base").unwrap();
    cat.mark_applied(&i).unwrap();
    let final_path = i.final_path.clone().unwrap();
    assert!(std::fs::metadata(&final_path).unwrap().permissions().readonly());
    // Manifest scritto accanto, prima del commit.
    let manifest = final_path.with_file_name("manifest.json");
    common::write_bytes(&manifest, b"{}").unwrap();
    let (mut cat, rep) = fx.crash_and_reopen(cat);
    assert_eq!(single(&rep).0, RecoveryOutcome::RolledBack);
    assert!(!final_path.exists());
    assert!(!manifest.exists());
    assert!(!final_path.parent().unwrap().exists(), "directory vuota rimossa");
    fx.assert_safe(&mut cat);
}

// ----------------------------------------------------------- cancellazione

fn delete_overlay_intent(fx: &Fixture, cat: &mut Catalog) -> (Intent, DiskId) {
    let run = common::run_from(cat, &fx.vm, &fx.baseline);
    // L'esecuzione referenzia l'overlay: per cancellarlo si usa un disco
    // overlay non referenziato da esecuzioni.
    let disk = DiskId::new_v7();
    let rid = RunId::new_v7();
    let rel = CatalogLayout::overlay(&fx.vm, &rid);
    let p = cat.layout().resolve(&rel).unwrap();
    common::write_bytes(&p, b"overlay conservato").unwrap();
    let row = fx.overlay_row(disk, &rid);
    cat.with_tx(|tx| tx.insert_disk(&row)).unwrap();
    let _ = run;
    let i = cat
        .begin_intent(IntentSpec {
            kind: IntentKind::DeleteFile,
            vm_id: Some(fx.vm),
            operation_id: None,
            target_kind: TargetKind::Disk,
            target_id: Some(disk.to_string()),
            final_path: Some(rel),
            read_only: false,
        })
        .unwrap();
    assert_eq!(
        cat.read(|tx| tx.disk(&disk)).unwrap().status,
        DiskStatus::Deleting
    );
    (i, disk)
}

#[test]
fn crash_before_delete_rename_restores_ready() {
    let (fx, mut cat) = fixture();
    let (i, disk) = delete_overlay_intent(&fx, &mut cat);
    let (mut cat, rep) = fx.crash_and_reopen(cat);
    assert_eq!(single(&rep), (RecoveryOutcome::RolledBack, "delete_not_started"));
    assert!(i.final_path.as_ref().unwrap().is_file());
    assert_eq!(
        cat.read(|tx| tx.disk(&disk)).unwrap().status,
        DiskStatus::Ready
    );
    fx.assert_safe(&mut cat);
}

#[test]
fn crash_after_delete_rename_rolls_forward() {
    for applied in [false, true] {
        let (fx, mut cat) = fixture();
        let (i, disk) = delete_overlay_intent(&fx, &mut cat);
        if applied {
            cat.mark_applied(&i).unwrap();
        } else {
            std::fs::rename(i.final_path.as_ref().unwrap(), &i.temp_path).unwrap();
        }
        let (mut cat, rep) = fx.crash_and_reopen(cat);
        assert_eq!(
            single(&rep),
            (RecoveryOutcome::RolledForward, "rolled_forward+temp_removed")
        );
        assert!(cat.read(|tx| tx.disk(&disk)).is_err());
        assert!(!i.final_path.as_ref().unwrap().exists());
        fx.assert_safe(&mut cat);
    }
}

#[test]
fn delete_refused_by_new_reference_restores_file() {
    let (fx, mut cat) = fixture();
    let (i, disk) = delete_overlay_intent(&fx, &mut cat);
    cat.mark_applied(&i).unwrap();
    // Un riferimento comparso dopo il controllo iniziale.
    let run = RunRecord {
        run_id: RunId::new_v7(),
        vm_id: fx.vm,
        baseline_id: Some(fx.baseline),
        overlay_disk_id: Some(disk),
        vars_fw_id: None,
        state: RunState::Stopped,
        started_at: now_rfc3339(),
        ended_at: Some(now_rfc3339()),
        termination_cause: None,
    };
    cat.with_tx(|tx| tx.insert_run(&run)).unwrap();
    let (mut cat, rep) = fx.crash_and_reopen(cat);
    assert_eq!(
        single(&rep),
        (RecoveryOutcome::RolledBack, "delete_refused+file_restored")
    );
    assert!(i.final_path.as_ref().unwrap().is_file());
    assert_eq!(
        cat.read(|tx| tx.disk(&disk)).unwrap().status,
        DiskStatus::Ready
    );
    fx.assert_safe(&mut cat);
}

#[test]
fn leftover_temp_after_committed_delete_is_removed() {
    let (fx, mut cat) = fixture();
    let (i, disk) = delete_overlay_intent(&fx, &mut cat);
    cat.mark_applied(&i).unwrap();
    cat.commit(&i, |tx| tx.delete_disk(&disk)).unwrap();
    assert!(!i.temp_path.exists());
    // Simula un crash tra la conferma e la rimozione del temporaneo.
    common::write_bytes(&i.temp_path, b"residuo").unwrap();
    let dbp = fx.dir.path().join(vm_storage::DB_FILE);
    drop(cat);
    let conn = vm_storage::migrate::open_connection(&dbp).unwrap();
    conn.execute(
        "UPDATE journal SET resolution = NULL WHERE intent_id = ?1",
        [i.id],
    )
    .unwrap();
    drop(conn);
    let mut cat = Catalog::open(fx.dir.path()).unwrap();
    let rep = cat.open_report().unwrap().recovery.clone();
    assert_eq!(rep.leftover_temps_removed, vec![i.id]);
    fx.assert_safe(&mut cat);
}

// ------------------------------------------------------------------ export

#[test]
fn crash_during_export_removes_temp_only() {
    let (fx, mut cat) = fixture();
    let i = cat
        .begin_intent(IntentSpec {
            kind: IntentKind::Export,
            vm_id: Some(fx.vm),
            operation_id: None,
            target_kind: TargetKind::None,
            target_id: None,
            final_path: None,
            read_only: false,
        })
        .unwrap();
    common::write_bytes(&i.temp_path, b"report").unwrap();
    let (mut cat, rep) = fx.crash_and_reopen(cat);
    assert_eq!(single(&rep), (RecoveryOutcome::RolledBack, "temp_removed"));
    fx.assert_safe(&mut cat);
}

#[test]
fn recover_is_idempotent() {
    let (fx, mut cat) = fixture();
    let (i, _, _) = fx.overlay_intent(&mut cat);
    write(&i.temp_path);
    cat.mark_applied(&i).unwrap();
    let first = cat.recover().unwrap();
    assert_eq!(first.intents.len(), 1);
    let second = cat.recover().unwrap();
    assert!(second.intents.is_empty());
    fx.assert_safe(&mut cat);
}

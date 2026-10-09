//! Porte verso le dipendenze esterne (ROADMAP 6.1, 6.3).
//!
//! Il core non esegue I/O: orologio, generazione di identificativi, catalogo,
//! audit, ispezione dei media e backend QEMU sono iniettati tramite questi trait.
//! Le implementazioni reali stanno in `vm-storage`, `qemu-backend` e
//! `lightvm-service`; i test usano implementazioni finte in memoria.
//!
//! I trait sono sincroni: un'implementazione asincrona viene adattata dal
//! supervisor (per esempio con un worker dedicato). Il percorso consigliato per
//! il backend è quello "senza I/O" di [`crate::Core::begin_dispatch`] /
//! [`crate::Core::finish_dispatch`], che non trattiene il core durante la
//! chiamata al backend; [`VmBackend`] serve agli helper e ai test.

use lightvm_proto::{
    AuditEvent, BaselineId, DisplayGeometry, GrantId, InputEvent, MediaEntry, MediaId, OperationId,
    OperationRecord, PublicError, RunEndReason, RunId, RunRecord, Screenshot, SessionId,
    UtcTimestamp, VmId, VmRecord, VmState,
};

use crate::media::{ObservedMediaFacts, VerifiedMedia};

/// Orologio UTC.
pub trait Clock {
    /// Istante corrente.
    fn now(&self) -> UtcTimestamp;
}

/// Sorgente di identificativi UUID v7.
pub trait IdGenerator {
    /// Millisecondi Unix e 10 byte di contatore/casualità per il prossimo identificativo.
    fn next_parts(&mut self) -> (u64, [u8; 10]);

    /// Nuovo [`VmId`].
    fn vm_id(&mut self) -> VmId {
        let (m, r) = self.next_parts();
        VmId::from_unix_millis(m, r)
    }
    /// Nuovo [`RunId`].
    fn run_id(&mut self) -> RunId {
        let (m, r) = self.next_parts();
        RunId::from_unix_millis(m, r)
    }
    /// Nuovo [`SessionId`].
    fn session_id(&mut self) -> SessionId {
        let (m, r) = self.next_parts();
        SessionId::from_unix_millis(m, r)
    }
    /// Nuovo [`GrantId`].
    fn grant_id(&mut self) -> GrantId {
        let (m, r) = self.next_parts();
        GrantId::from_unix_millis(m, r)
    }
    /// Nuovo [`MediaId`].
    fn media_id(&mut self) -> MediaId {
        let (m, r) = self.next_parts();
        MediaId::from_unix_millis(m, r)
    }
    /// Nuovo [`OperationId`] (per operazioni generate dal supervisor).
    fn operation_id(&mut self) -> OperationId {
        let (m, r) = self.next_parts();
        OperationId::from_unix_millis(m, r)
    }
    /// Nuovo [`BaselineId`].
    fn baseline_id(&mut self) -> BaselineId {
        let (m, r) = self.next_parts();
        BaselineId::from_unix_millis(m, r)
    }
}

/// Catalogo persistente (implementato da `vm-storage` su SQLite).
pub trait CatalogStore {
    /// Inserisce una nuova VM.
    fn insert_vm(&mut self, vm: &VmRecord) -> Result<(), PublicError>;
    /// Legge una VM.
    fn get_vm(&self, id: VmId) -> Result<Option<VmRecord>, PublicError>;
    /// Elenca le VM.
    fn list_vms(&self) -> Result<Vec<VmRecord>, PublicError>;
    /// Aggiorna lo stato persistito (indicativo).
    fn set_persisted_state(&mut self, id: VmId, state: VmState) -> Result<(), PublicError>;
    /// Inserisce una voce del registro media.
    fn insert_media(&mut self, entry: &MediaEntry) -> Result<(), PublicError>;
    /// Legge una voce del registro media.
    fn get_media(&self, id: MediaId) -> Result<Option<MediaEntry>, PublicError>;
    /// Inserisce o aggiorna un'operazione.
    fn upsert_operation(&mut self, op: &OperationRecord) -> Result<(), PublicError>;
    /// Legge un'operazione.
    fn get_operation(&self, id: OperationId) -> Result<Option<OperationRecord>, PublicError>;
    /// Inserisce un'esecuzione.
    fn insert_run(&mut self, run: &RunRecord) -> Result<(), PublicError>;
    /// Chiude un'esecuzione.
    fn finish_run(
        &mut self,
        id: RunId,
        ended_at: UtcTimestamp,
        reason: RunEndReason,
    ) -> Result<(), PublicError>;
}

/// Destinazione degli eventi di audit, in ordine di `seq`.
pub trait AuditSink {
    /// Aggiunge un evento. Gli eventi non contengono payload sensibili.
    fn append(&mut self, event: &AuditEvent) -> Result<(), PublicError>;
}

/// Ispezione di una copia gestita del registro media: calcola hash, dimensione e
/// identità OS sul file aperto in modo protetto (implementata da `vm-storage` o
/// `qemu-backend`). Il confronto con il registro è del core ([`crate::media::verify_media`]).
pub trait MediaInspector {
    /// Osserva la copia gestita della voce.
    fn inspect(&mut self, entry: &MediaEntry) -> Result<ObservedMediaFacts, PublicError>;
}

/// Backend QEMU con API ridotta (ROADMAP 6.3). Contratti:
/// - `start` ritorna `Ok` solo con processo vivo e QMP pronto;
/// - `shutdown` e `force_stop` ritornano `Ok` solo dopo aver osservato la fine del processo;
/// - `force_stop` usa primitive OS e non dipende da QMP (ADR-0009);
/// - nessun metodo riceve percorsi o opzioni dal client.
pub trait VmBackend {
    /// Avvia l'esecuzione `run_id` con i soli media verificati dal core.
    fn start(
        &mut self,
        vm: &VmRecord,
        run_id: RunId,
        media: &[VerifiedMedia],
    ) -> Result<(), PublicError>;
    /// Arresto ordinario.
    fn shutdown(&mut self, vm_id: VmId, run_id: RunId) -> Result<(), PublicError>;
    /// Arresto forzato dell'albero di processi con primitive OS.
    fn force_stop(&mut self, vm_id: VmId, run_id: RunId) -> Result<(), PublicError>;
    /// Reset del guest.
    fn reset(&mut self, vm_id: VmId, run_id: RunId) -> Result<(), PublicError>;
    /// Screenshot entro `max_bytes`.
    fn screenshot(
        &mut self,
        vm_id: VmId,
        run_id: RunId,
        max_bytes: u64,
    ) -> Result<Screenshot, PublicError>;
    /// Inoltra eventi di input già autorizzati. Mai ripetuti automaticamente.
    fn send_input(
        &mut self,
        vm_id: VmId,
        run_id: RunId,
        geometry: DisplayGeometry,
        events: &[InputEvent],
    ) -> Result<(), PublicError>;
}

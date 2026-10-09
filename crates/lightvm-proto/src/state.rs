//! Macchina a stati della VM (ROADMAP 6.3; `docs/ARCHITECTURE.md` sezione 4).
//!
//! La tabella delle transizioni è una funzione pura ([`VmState::can_transition_to`]):
//! le guardie applicative (coda, sessioni, `run_id`) stanno in `vm-core`.
//! Lo stato *persistito* nel database è un'indicazione; lo stato *osservato*
//! ([`ObservedState`]) deriva da processo e QMP e prevale.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{RunId, UtcTimestamp};

/// Stato del ciclo di vita di una VM.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum VmState {
    /// Processo assente e file rilasciati. Unico stato per operazioni su dischi, NVRAM e media.
    Stopped,
    /// Spawn avviato, nuovo `run_id` assegnato, QMP non ancora pronto.
    Starting,
    /// Processo vivo e QMP pronto.
    Running,
    /// Arresto richiesto; completo solo a fine processo osservata.
    Stopping,
    /// Errore di avvio o uscita inattesa. Nessuna promozione automatica a baseline.
    Failed,
    /// Riconciliazione dopo crash (journal, file, processi): nessuna mutazione esterna.
    Recovering,
}

impl VmState {
    /// Tutti gli stati, in ordine di dichiarazione.
    pub const ALL: [VmState; 6] = [
        VmState::Stopped,
        VmState::Starting,
        VmState::Running,
        VmState::Stopping,
        VmState::Failed,
        VmState::Recovering,
    ];

    /// Stati raggiungibili direttamente da `self`. Funzione pura, tabella congelata:
    ///
    /// | Da | A |
    /// | --- | --- |
    /// | `stopped` | `starting` |
    /// | `starting` | `running`, `stopping` (force-stop, Emergency Stop), `failed`, `recovering` |
    /// | `running` | `stopping`, `failed` (uscita inattesa), `recovering` |
    /// | `stopping` | `stopped`, `failed`, `recovering` |
    /// | `failed` | `recovering` |
    /// | `recovering` | `stopped`, `failed` |
    ///
    /// L'ingresso in `recovering` da `starting`/`running`/`stopping` copre la
    /// riconciliazione dopo un crash del supervisor, quando lo stato persistito
    /// non è più confermato da processo e QMP. Nessuna transizione riflessiva.
    #[must_use]
    pub const fn allowed_transitions(self) -> &'static [VmState] {
        match self {
            VmState::Stopped => &[VmState::Starting],
            VmState::Starting => &[
                VmState::Running,
                VmState::Stopping,
                VmState::Failed,
                VmState::Recovering,
            ],
            VmState::Running => &[VmState::Stopping, VmState::Failed, VmState::Recovering],
            VmState::Stopping => &[VmState::Stopped, VmState::Failed, VmState::Recovering],
            VmState::Failed => &[VmState::Recovering],
            VmState::Recovering => &[VmState::Stopped, VmState::Failed],
        }
    }

    /// `true` se la transizione `self → to` è ammessa dalla tabella.
    #[must_use]
    pub fn can_transition_to(self, to: VmState) -> bool {
        self.allowed_transitions().contains(&to)
    }

    /// `true` se esiste (o può esistere) un processo QEMU di questa VM.
    #[must_use]
    pub const fn has_process(self) -> bool {
        matches!(self, VmState::Starting | VmState::Running | VmState::Stopping)
    }

    /// Nome stabile in `snake_case` (uguale alla serializzazione).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            VmState::Stopped => "stopped",
            VmState::Starting => "starting",
            VmState::Running => "running",
            VmState::Stopping => "stopping",
            VmState::Failed => "failed",
            VmState::Recovering => "recovering",
        }
    }
}

impl std::fmt::Display for VmState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Origine di un'osservazione di stato.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ObservationSource {
    /// Handle del processo QEMU (vivo, terminato, codice d'uscita).
    Process,
    /// Risposta o evento QMP (`query-status`, `SHUTDOWN`, ...).
    Qmp,
    /// Valore persistito nel catalogo: indicativo, mai sufficiente da solo.
    Database,
}

/// Stato osservato di una VM, con origine e istante.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ObservedState {
    /// Stato osservato.
    pub state: VmState,
    /// Origine dell'osservazione.
    pub source: ObservationSource,
    /// Esecuzione a cui l'osservazione si riferisce, se esiste un processo.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<RunId>,
    /// Istante dell'osservazione.
    pub observed_at: UtcTimestamp,
}

impl ObservedState {
    /// `true` se l'osservazione proviene da processo o QMP, cioè può confermare lo stato.
    #[must_use]
    pub fn is_authoritative(&self) -> bool {
        !matches!(self.source, ObservationSource::Database)
    }
}

/// Evento elementare osservato dal backend, usato dal core per derivare lo stato.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum BackendObservation {
    /// Processo creato e associato all'esecuzione.
    ProcessSpawned {
        /// Esecuzione.
        run_id: RunId,
    },
    /// QMP pronto (capabilities negoziate) e processo vivo.
    QmpReady {
        /// Esecuzione.
        run_id: RunId,
    },
    /// Fine processo osservata tramite handle (non tramite PID).
    ProcessExited {
        /// Esecuzione.
        run_id: RunId,
        /// `true` se l'uscita era stata richiesta (stop ordinario, force-stop, Emergency Stop).
        expected: bool,
    },
    /// Avvio fallito prima che il processo fosse pronto.
    StartFailed {
        /// Esecuzione.
        run_id: RunId,
    },
}

impl BackendObservation {
    /// Esecuzione a cui l'osservazione si riferisce.
    #[must_use]
    pub fn run_id(&self) -> RunId {
        match self {
            Self::ProcessSpawned { run_id }
            | Self::QmpReady { run_id }
            | Self::ProcessExited { run_id, .. }
            | Self::StartFailed { run_id } => *run_id,
        }
    }
}

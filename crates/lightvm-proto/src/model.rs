//! Modelli del catalogo: VM, media, baseline, esecuzioni, operazioni, limiti
//! (ROADMAP 4, 6.3, 7.2, 8).

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{
    BaselineId, ErrorCode, Label, ManagedRelPath, MediaId, OperationId, RunId, SessionId,
    Sha256Digest, UtcTimestamp, VmId, VmName, VmState,
};

/// Sistema operativo guest dichiarato. Nel MVP il riferimento è Ubuntu LTS
/// x86_64 con versione e checksum fissati in F0 (ROADMAP 4).
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum GuestOs {
    /// Ubuntu LTS x86_64 (guest di riferimento del MVP).
    UbuntuLtsX86_64,
    /// Altro Linux x86_64: gestibile manualmente, senza garanzie di compatibilità.
    OtherLinuxX86_64,
}

/// Profilo di rete. Nel MVP l'unico valore ammesso è `offline` (ADR-0004): nessuna
/// NIC guest, periferiche di rete predefinite escluse, nessun forwarding.
#[derive(
    Clone,
    Copy,
    Debug,
    Default,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    Serialize,
    Deserialize,
    JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum NetworkProfile {
    /// Nessuna NIC.
    #[default]
    Offline,
}

/// Firmware del guest. Nel MVP solo UEFI con stato NVRAM gestito (ROADMAP 8).
#[derive(
    Clone,
    Copy,
    Debug,
    Default,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    Serialize,
    Deserialize,
    JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum Firmware {
    /// UEFI (OVMF) con NVRAM per VM.
    #[default]
    Uefi,
}

/// Configurazione di una VM richiesta dall'utente. Nessun campo è un percorso o
/// un'opzione QEMU: i media sono riferiti per [`MediaId`] del registro approvato.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct VmConfig {
    /// Nome visibile.
    pub name: VmName,
    /// Sistema operativo guest dichiarato.
    pub guest_os: GuestOs,
    /// Numero di vCPU (non è un limite percentuale di CPU: ROADMAP 7.2).
    pub vcpus: u32,
    /// Memoria guest in MiB (non è il limite dell'intero processo QEMU).
    pub memory_mib: u64,
    /// Dimensione virtuale del disco principale QCOW2 in GiB.
    pub disk_gib: u64,
    /// Profilo di rete; nel MVP solo `offline`.
    #[serde(default)]
    pub network: NetworkProfile,
    /// Supporto di installazione dal registro media (tipo `install_iso`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub install_media: Option<MediaId>,
    /// Supporto dati opzionale in sola lettura (tipo `data_iso`), montato a VM spenta.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data_media: Option<MediaId>,
    /// Firmware; nel MVP solo `uefi`.
    #[serde(default)]
    pub firmware: Firmware,
    /// Limiti di esecuzione applicati dal supervisor al processo QEMU
    /// (ADR-0017); assenti nei cataloghi precedenti → valori predefiniti.
    #[serde(default)]
    pub limits: VmLimits,
}

/// Dimensione massima di un'ISO di installazione nel registro media (16 GiB;
/// ADR-0008).
pub const MAX_INSTALL_MEDIA_BYTES: u64 = 16 << 30;
/// Dimensione massima di un'ISO dati in sola lettura (8 GiB, DVD a doppio
/// strato; ADR-0008). Conta anche nel budget di storage (ADR-0017).
pub const MAX_DATA_MEDIA_BYTES: u64 = 8 << 30;

impl MediaKind {
    /// Dimensione massima ammessa per questo tipo di supporto.
    #[must_use]
    pub const fn max_bytes(self) -> u64 {
        match self {
            MediaKind::InstallIso => MAX_INSTALL_MEDIA_BYTES,
            MediaKind::DataIso => MAX_DATA_MEDIA_BYTES,
        }
    }
}

/// Tetto CPU minimo ammesso, in percentuale della capacità dei vCPU.
pub const MIN_CPU_MAX_PERCENT: u8 = 5;
/// Margine di memoria minimo oltre la RAM guest, in MiB.
pub const MIN_MEMORY_MARGIN_MIB: u32 = 256;
/// Margine di memoria massimo oltre la RAM guest, in MiB.
pub const MAX_MEMORY_MARGIN_MIB: u32 = 16_384;
/// Budget di storage massimo per VM, in GiB.
pub const MAX_STORAGE_BUDGET_GIB: u64 = 16_384;

/// Limiti di esecuzione di una VM (ADR-0017), impostati dall'utente nelle
/// impostazioni della VM e applicati dal supervisor, mai dal client.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct VmLimits {
    /// Tetto di CPU in percentuale della capacità dei vCPU assegnati
    /// ([`MIN_CPU_MAX_PERCENT`]..=100; 100 = nessun tetto). Applicato dal
    /// sistema operativo all'intero processo QEMU.
    pub cpu_max_percent: u8,
    /// Memoria concessa al processo QEMU oltre la RAM del guest, in MiB
    /// ([`MIN_MEMORY_MARGIN_MIB`]..=[`MAX_MEMORY_MARGIN_MIB`]). Superato il
    /// limite, QEMU è terminato e l'uscita è classificata come limite di risorse.
    pub memory_margin_mib: u32,
    /// Spazio massimo in GiB per i file della VM oltre al disco principale
    /// (overlay, baseline, variabili firmware); `None` = nessun budget dedicato.
    /// Verificato a ogni avvio.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub storage_budget_gib: Option<u64>,
}

impl Default for VmLimits {
    fn default() -> Self {
        Self {
            cpu_max_percent: 100,
            memory_margin_mib: 1024,
            storage_budget_gib: None,
        }
    }
}

impl VmLimits {
    /// Validazione strutturale dei limiti.
    pub fn validate(&self) -> Result<(), crate::PublicError> {
        if !(MIN_CPU_MAX_PERCENT..=100).contains(&self.cpu_max_percent) {
            return Err(crate::PublicError::invalid_argument(format!(
                "cpu_max_percent fuori dall'intervallo {MIN_CPU_MAX_PERCENT}..=100"
            )));
        }
        if !(MIN_MEMORY_MARGIN_MIB..=MAX_MEMORY_MARGIN_MIB).contains(&self.memory_margin_mib) {
            return Err(crate::PublicError::invalid_argument(format!(
                "memory_margin_mib fuori dall'intervallo {MIN_MEMORY_MARGIN_MIB}..={MAX_MEMORY_MARGIN_MIB}"
            )));
        }
        if let Some(b) = self.storage_budget_gib
            && !(1..=MAX_STORAGE_BUDGET_GIB).contains(&b)
        {
            return Err(crate::PublicError::invalid_argument(format!(
                "storage_budget_gib fuori dall'intervallo 1..={MAX_STORAGE_BUDGET_GIB}"
            )));
        }
        Ok(())
    }
}

/// VM del catalogo con revisione della configurazione e ultimo stato persistito.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct VmRecord {
    /// Identificativo.
    pub id: VmId,
    /// Configurazione corrente.
    pub config: VmConfig,
    /// Revisione della configurazione, incrementata a ogni modifica (controllo delle revisioni).
    pub config_revision: u64,
    /// Ultimo stato persistito: indicativo, lo stato osservato prevale.
    pub persisted_state: VmState,
    /// Istante di creazione.
    pub created_at: UtcTimestamp,
}

/// Tipo di supporto nel registro media.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum MediaKind {
    /// ISO di installazione del guest.
    InstallIso,
    /// ISO dati in sola lettura per portare nel guest il software da testare (ADR-0008).
    DataIso,
}

/// Supporto approvato, come visto dai client: nessun percorso host.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct MediaRecord {
    /// Identificativo.
    pub id: MediaId,
    /// Tipo di supporto.
    pub kind: MediaKind,
    /// SHA-256 della copia gestita, calcolato all'approvazione.
    pub sha256: Sha256Digest,
    /// Dimensione in byte della copia gestita.
    pub size_bytes: u64,
    /// Origine dichiarata dall'utente (testo, mai interpretato).
    pub declared_origin: Label,
    /// Istante dell'approvazione.
    pub approved_at: UtcTimestamp,
}

/// Identità del file gestito secondo l'OS (per esempio numero di serie del
/// volume e indice del file su Windows, `st_dev`/`st_ino` su Unix). Rileva la
/// sostituzione del file anche quando hash e dimensione non sono ancora ricalcolati.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct FileIdentity {
    /// Volume o dispositivo.
    pub volume: u64,
    /// Identificativo del file nel volume (128 bit, come `FILE_ID_INFO` su Windows;
    /// su Unix l'inode nei primi 8 byte big-endian).
    pub file_id: [u8; 16],
}

/// Voce completa del registro media, **solo interna** al supervisor e allo
/// storage: contiene il percorso gestito opaco. Non ha schema pubblico.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MediaEntry {
    /// Dati pubblici.
    pub record: MediaRecord,
    /// Percorso relativo della copia gestita in sola lettura.
    pub managed_path: ManagedRelPath,
    /// Identità OS registrata all'approvazione, se disponibile.
    pub file_identity: Option<FileIdentity>,
}

/// Origine di una baseline (ROADMAP 8).
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum BaselineOrigin {
    /// Registrata dopo arresto corretto osservato.
    CleanShutdown,
    /// Checkpoint materializzato da un overlay a VM spenta.
    Checkpoint,
}

/// Baseline a VM spenta: disco autonomo più stato firmware, immutabile.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct BaselineRecord {
    /// Identificativo.
    pub id: BaselineId,
    /// VM di appartenenza.
    pub vm_id: VmId,
    /// Revisione della configurazione VM registrata con la baseline.
    pub config_revision: u64,
    /// Origine.
    pub origin: BaselineOrigin,
    /// SHA-256 dell'immagine disco della baseline.
    pub disk_sha256: Sha256Digest,
    /// SHA-256 dello stato firmware (NVRAM) della baseline.
    pub firmware_sha256: Sha256Digest,
    /// Baseline iniziale protetta: non cancellabile né sostituibile dai tool agente.
    pub protected: bool,
    /// Etichetta opzionale.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<Label>,
    /// Istante di creazione.
    pub created_at: UtcTimestamp,
}

/// Motivo della fine di un'esecuzione.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum RunEndReason {
    /// Arresto ordinario completato (fine processo osservata).
    Stopped,
    /// Arresto forzato richiesto (stato non pulito).
    ForceStopped,
    /// Emergency Stop.
    EmergencyStop,
    /// Avvio fallito.
    StartFailed,
    /// Uscita inattesa del processo.
    UnexpectedExit,
    /// Esecuzione chiusa dalla riconciliazione dopo crash del supervisor.
    Reconciled,
}

/// Esecuzione: un processo QEMU dall'avvio alla fine osservata.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RunRecord {
    /// Identificativo.
    pub id: RunId,
    /// VM.
    pub vm_id: VmId,
    /// Revisione della configurazione usata per l'avvio.
    pub config_revision: u64,
    /// Baseline di partenza (da F3).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub baseline_id: Option<BaselineId>,
    /// Istante di avvio.
    pub started_at: UtcTimestamp,
    /// Istante di fine osservata.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<UtcTimestamp>,
    /// Motivo della fine.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_reason: Option<RunEndReason>,
}

/// Tipo di operazione della coda per VM.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum OperationKind {
    /// Creazione della VM nel catalogo.
    Create,
    /// Avvio.
    Start,
    /// Arresto ordinario.
    Stop,
    /// Arresto forzato.
    ForceStop,
    /// Reset del guest.
    Reset,
    /// Cattura dello schermo.
    Screenshot,
    /// Input tipizzato.
    Input,
    /// Checkpoint a VM spenta (da F3).
    Snapshot,
    /// Ripristino di una baseline a VM spenta (da F3).
    Restore,
    /// Emergency Stop (percorso prioritario, fuori coda).
    EmergencyStop,
}

impl OperationKind {
    /// `true` per le mutazioni del ciclo di vita: al massimo una in corso o in coda per VM.
    #[must_use]
    pub const fn is_lifecycle_mutation(self) -> bool {
        matches!(
            self,
            OperationKind::Create
                | OperationKind::Start
                | OperationKind::Stop
                | OperationKind::ForceStop
                | OperationKind::Reset
                | OperationKind::Snapshot
                | OperationKind::Restore
                | OperationKind::EmergencyStop
        )
    }

    /// `true` se ripetere l'operazione con lo stesso effetto è sicuro (retry ammesso).
    /// Input e reset non lo sono (ROADMAP 6.3).
    #[must_use]
    pub const fn is_idempotent(self) -> bool {
        matches!(
            self,
            OperationKind::Start
                | OperationKind::Stop
                | OperationKind::ForceStop
                | OperationKind::Screenshot
                | OperationKind::EmergencyStop
        )
    }
}

/// Stato di un'operazione.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum OperationStatus {
    /// Accettata, in coda, non ancora inoltrata al backend.
    Queued,
    /// Inoltrata al backend: può produrre effetti anche dopo una revoca.
    Forwarded,
    /// Completata con successo.
    Succeeded,
    /// Inoltrata e fallita.
    Failed,
    /// Scartata prima dell'inoltro (revoca, Emergency Stop, `run_id` superato): nessun effetto.
    Discarded,
}

impl OperationStatus {
    /// `true` se lo stato è finale.
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(
            self,
            OperationStatus::Succeeded | OperationStatus::Failed | OperationStatus::Discarded
        )
    }
}

/// Registro di un'operazione (consultabile per `operation_id`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct OperationRecord {
    /// Identificativo (chiave di idempotenza).
    pub id: OperationId,
    /// VM.
    pub vm_id: VmId,
    /// Esecuzione a cui si riferisce, se pertinente.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<RunId>,
    /// Sessione richiedente, se agente.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<SessionId>,
    /// Tipo.
    pub kind: OperationKind,
    /// Stato.
    pub status: OperationStatus,
    /// Codice d'errore se `failed` o `discarded`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<ErrorCode>,
    /// Istante di accettazione.
    pub requested_at: UtcTimestamp,
    /// Istante dell'ultimo cambio di stato.
    pub updated_at: UtcTimestamp,
}

/// Limiti di risorse (globali o per VM). Tutti i campi sono massimi inclusivi.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub struct ResourceLimits {
    /// vCPU massime per VM.
    pub max_vcpus: u32,
    /// Memoria guest massima per VM in MiB.
    pub max_memory_mib: u64,
    /// Disco virtuale massimo per VM in GiB.
    pub max_disk_gib: u64,
    /// VM in esecuzione contemporanea (MVP: 1).
    pub max_running_vms: u32,
    /// Sessioni agente contemporanee (MVP: 1).
    pub max_agent_sessions: u32,
}

impl ResourceLimits {
    /// Limiti del MVP: una VM in esecuzione e una sessione agente (ROADMAP 4);
    /// massimi per VM prudenti, da tarare con le misure di F0 (ADR-0017).
    pub const MVP_DEFAULT: ResourceLimits = ResourceLimits {
        max_vcpus: 8,
        max_memory_mib: 16 * 1024,
        max_disk_gib: 256,
        max_running_vms: 1,
        max_agent_sessions: 1,
    };

    /// Intersezione: per ogni campo il minimo. Nessun lato amplia l'altro.
    #[must_use]
    pub fn intersect(&self, other: &ResourceLimits) -> ResourceLimits {
        ResourceLimits {
            max_vcpus: self.max_vcpus.min(other.max_vcpus),
            max_memory_mib: self.max_memory_mib.min(other.max_memory_mib),
            max_disk_gib: self.max_disk_gib.min(other.max_disk_gib),
            max_running_vms: self.max_running_vms.min(other.max_running_vms),
            max_agent_sessions: self.max_agent_sessions.min(other.max_agent_sessions),
        }
    }

    /// `true` se ogni campo di `self` è ≤ del corrispondente di `other`.
    #[must_use]
    pub fn is_within(&self, other: &ResourceLimits) -> bool {
        self.max_vcpus <= other.max_vcpus
            && self.max_memory_mib <= other.max_memory_mib
            && self.max_disk_gib <= other.max_disk_gib
            && self.max_running_vms <= other.max_running_vms
            && self.max_agent_sessions <= other.max_agent_sessions
    }
}

/// Capacità dell'host rilevate dal probe (fornite al core, mai inventate).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub struct HostCapacity {
    /// Processori logici.
    pub logical_cpus: u32,
    /// Memoria fisica totale in MiB.
    pub total_memory_mib: u64,
    /// Memoria da lasciare all'host in MiB (riserva).
    pub reserved_memory_mib: u64,
    /// Spazio libero sul volume del catalogo in GiB.
    pub free_storage_gib: u64,
    /// Spazio da lasciare libero in GiB (riserva, ADR-0017).
    pub reserved_storage_gib: u64,
    /// Acceleratore hardware disponibile e approvato (nessun fallback TCG: ADR-0007).
    pub accelerator_available: bool,
    /// Cattura dello schermo disponibile nel backend.
    pub screenshot_available: bool,
    /// Input tipizzato disponibile nel backend.
    pub input_available: bool,
    /// Baseline e ripristino disponibili (da F3).
    pub baseline_available: bool,
}

impl HostCapacity {
    /// Limiti derivati dalla capacità host: vCPU ≤ processori logici, memoria ≤
    /// totale meno riserva, disco ≤ libero meno riserva.
    #[must_use]
    pub fn as_limits(&self) -> ResourceLimits {
        ResourceLimits {
            max_vcpus: self.logical_cpus,
            max_memory_mib: self
                .total_memory_mib
                .saturating_sub(self.reserved_memory_mib),
            max_disk_gib: self
                .free_storage_gib
                .saturating_sub(self.reserved_storage_gib),
            max_running_vms: if self.accelerator_available {
                u32::MAX
            } else {
                0
            },
            max_agent_sessions: u32::MAX,
        }
    }
}

/// Geometria del display guest in pixel.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub struct DisplayGeometry {
    /// Larghezza (1..=8192).
    #[schemars(range(min = 1, max = 8192))]
    pub width: u32,
    /// Altezza (1..=8192).
    #[schemars(range(min = 1, max = 8192))]
    pub height: u32,
}

impl DisplayGeometry {
    /// Lato massimo ammesso.
    pub const MAX_SIDE: u32 = 8192;

    /// Valida e costruisce.
    pub fn new(width: u32, height: u32) -> Result<Self, crate::PublicError> {
        let g = Self { width, height };
        g.validate()?;
        Ok(g)
    }

    /// Verifica i limiti (utile dopo la deserializzazione).
    pub fn validate(&self) -> Result<(), crate::PublicError> {
        if (1..=Self::MAX_SIDE).contains(&self.width) && (1..=Self::MAX_SIDE).contains(&self.height)
        {
            Ok(())
        } else {
            Err(crate::PublicError::invalid_argument(
                "geometria del display fuori da 1..=8192",
            ))
        }
    }
}

/// Formato dell'immagine di uno screenshot.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ImageFormat {
    /// PNG.
    Png,
}

/// Metadati di uno screenshot prodotto dal backend (ROADMAP 6.4, 9). I byte
/// dell'immagine viaggiano separatamente (contenuto immagine MCP o blocco IPC),
/// mai come percorso host.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Screenshot {
    /// VM.
    pub vm_id: VmId,
    /// Esecuzione in cui è stato catturato.
    pub run_id: RunId,
    /// Geometria del display al momento della cattura.
    pub geometry: DisplayGeometry,
    /// Formato.
    pub format: ImageFormat,
    /// SHA-256 dei byte dell'immagine.
    pub sha256: Sha256Digest,
    /// Dimensione in byte dell'immagine.
    pub byte_len: u64,
    /// Istante della cattura.
    pub captured_at: UtcTimestamp,
}

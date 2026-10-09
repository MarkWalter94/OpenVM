//! Sessioni agente, grant, lease, capability, esiti dei test (ROADMAP 7.3, 9).

use std::borrow::Cow;
use std::collections::BTreeSet;
use std::fmt;

use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::{GrantId, OperationId, PublicError, SessionId, UtcTimestamp, VmId};

/// Azione autorizzabile su una VM. I nomi serializzati seguono il catalogo MCP
/// (ROADMAP 9); `vm.input` copre mouse e tastiera.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema)]
pub enum Capability {
    /// Elenco filtrato dal grant.
    #[serde(rename = "vm.list")]
    VmList,
    /// Stato.
    #[serde(rename = "vm.status")]
    VmStatus,
    /// Avvio.
    #[serde(rename = "vm.start")]
    VmStart,
    /// Arresto (ordinario o forzato).
    #[serde(rename = "vm.stop")]
    VmStop,
    /// Reset: richiede capability esplicita.
    #[serde(rename = "vm.reset")]
    VmReset,
    /// Screenshot.
    #[serde(rename = "vm.screenshot")]
    VmScreenshot,
    /// Mouse e tastiera.
    #[serde(rename = "vm.input")]
    VmInput,
    /// Checkpoint a VM spenta: richiede capability esplicita.
    #[serde(rename = "vm.snapshot")]
    VmSnapshot,
    /// Ripristino a VM spenta.
    #[serde(rename = "vm.restore")]
    VmRestore,
    /// Eventi LightVM e log QEMU autorizzati.
    #[serde(rename = "vm.logs")]
    VmLogs,
}

impl Capability {
    /// Tutte le capability.
    pub const ALL: [Capability; 10] = [
        Capability::VmList,
        Capability::VmStatus,
        Capability::VmStart,
        Capability::VmStop,
        Capability::VmReset,
        Capability::VmScreenshot,
        Capability::VmInput,
        Capability::VmSnapshot,
        Capability::VmRestore,
        Capability::VmLogs,
    ];

    /// Nome stabile.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Capability::VmList => "vm.list",
            Capability::VmStatus => "vm.status",
            Capability::VmStart => "vm.start",
            Capability::VmStop => "vm.stop",
            Capability::VmReset => "vm.reset",
            Capability::VmScreenshot => "vm.screenshot",
            Capability::VmInput => "vm.input",
            Capability::VmSnapshot => "vm.snapshot",
            Capability::VmRestore => "vm.restore",
            Capability::VmLogs => "vm.logs",
        }
    }

    /// `true` se la capability autorizza una mutazione (bloccata dalla pausa agente).
    #[must_use]
    pub const fn is_mutation(self) -> bool {
        matches!(
            self,
            Capability::VmStart
                | Capability::VmStop
                | Capability::VmReset
                | Capability::VmInput
                | Capability::VmSnapshot
                | Capability::VmRestore
        )
    }
}

impl fmt::Display for Capability {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Insieme ordinato di capability.
pub type CapabilitySet = BTreeSet<Capability>;

/// Quote di una sessione o di un livello di policy. Ogni campo è un massimo
/// inclusivo; l'intersezione prende il minimo.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub struct Quotas {
    /// Operazioni totali ammesse nella sessione.
    pub max_operations: u32,
    /// Eventi di input totali ammessi nella sessione.
    pub max_input_events: u32,
    /// Screenshot totali ammessi nella sessione.
    pub max_screenshots: u32,
    /// Dimensione massima di un singolo screenshot in byte.
    pub max_screenshot_bytes: u64,
    /// Operazioni in coda per VM non ancora inoltrate.
    pub max_queued_operations: u32,
}

impl Quotas {
    /// Quote senza restrizioni proprie (elemento neutro dell'intersezione).
    pub const UNBOUNDED: Quotas = Quotas {
        max_operations: u32::MAX,
        max_input_events: u32::MAX,
        max_screenshots: u32::MAX,
        max_screenshot_bytes: u64::MAX,
        max_queued_operations: u32::MAX,
    };

    /// Intersezione campo per campo (minimo).
    #[must_use]
    pub fn intersect(&self, other: &Quotas) -> Quotas {
        Quotas {
            max_operations: self.max_operations.min(other.max_operations),
            max_input_events: self.max_input_events.min(other.max_input_events),
            max_screenshots: self.max_screenshots.min(other.max_screenshots),
            max_screenshot_bytes: self.max_screenshot_bytes.min(other.max_screenshot_bytes),
            max_queued_operations: self.max_queued_operations.min(other.max_queued_operations),
        }
    }

    /// `true` se ogni campo di `self` è ≤ del corrispondente di `other`.
    #[must_use]
    pub fn is_within(&self, other: &Quotas) -> bool {
        self.max_operations <= other.max_operations
            && self.max_input_events <= other.max_input_events
            && self.max_screenshots <= other.max_screenshots
            && self.max_screenshot_bytes <= other.max_screenshot_bytes
            && self.max_queued_operations <= other.max_queued_operations
    }
}

/// Revisione monotona della policy applicata (registrata nei report, ROADMAP 9).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(transparent)]
pub struct PolicyRevision(pub u64);

/// Un livello di policy: capability ammesse e quote. Usato per limiti globali,
/// policy VM, grant di sessione e capacità host; la policy effettiva è
/// l'intersezione dei quattro livelli (ROADMAP 6.3, calcolata in `vm-core`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PolicyLayer {
    /// Capability ammesse da questo livello.
    pub capabilities: CapabilitySet,
    /// Quote di questo livello.
    pub quotas: Quotas,
}

impl PolicyLayer {
    /// Livello neutro: tutte le capability, quote illimitate.
    #[must_use]
    pub fn permissive() -> Self {
        Self {
            capabilities: Capability::ALL.into_iter().collect(),
            quotas: Quotas::UNBOUNDED,
        }
    }

    /// Livello che non ammette nulla.
    #[must_use]
    pub fn deny_all() -> Self {
        Self {
            capabilities: CapabilitySet::new(),
            quotas: Quotas {
                max_operations: 0,
                max_input_events: 0,
                max_screenshots: 0,
                max_screenshot_bytes: 0,
                max_queued_operations: 0,
            },
        }
    }
}

/// Policy effettiva risultante dall'intersezione, con la revisione applicata.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct EffectivePolicy {
    /// Capability effettive.
    pub capabilities: CapabilitySet,
    /// Quote effettive.
    pub quotas: Quotas,
    /// Revisione della policy applicata.
    pub revision: PolicyRevision,
}

/// Grant emesso dal supervisor dopo l'approvazione dell'utente nell'app (ROADMAP 9).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Grant {
    /// Identificativo.
    pub id: GrantId,
    /// Sessione a cui è legato.
    pub session_id: SessionId,
    /// VM ammesse (allowlist esplicita; vuota = nessuna VM).
    pub vm_allowlist: BTreeSet<VmId>,
    /// Capability concesse.
    pub capabilities: CapabilitySet,
    /// Scadenza assoluta UTC.
    pub expires_at: UtcTimestamp,
    /// Quote concesse.
    pub quotas: Quotas,
    /// Revisione della policy con cui il grant è stato emesso.
    pub policy_revision: PolicyRevision,
}

impl Grant {
    /// Livello di policy corrispondente al grant.
    #[must_use]
    pub fn as_layer(&self) -> PolicyLayer {
        PolicyLayer {
            capabilities: self.capabilities.clone(),
            quotas: self.quotas,
        }
    }
}

/// Durata massima di un lease senza rinnovo valido (ROADMAP 7.3: ≤ 10 s).
pub const LEASE_MAX_MILLIS: i64 = 10_000;

/// Lease di una sessione: va rinnovato prima della scadenza, altrimenti la
/// sessione è revocata con il percorso di emergenza (ROADMAP 7.3).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Lease {
    /// Sessione.
    pub session_id: SessionId,
    /// Ultimo rinnovo valido.
    pub renewed_at: UtcTimestamp,
    /// Scadenza: `renewed_at + durata`, con durata in `1..=LEASE_MAX_MILLIS`.
    pub expires_at: UtcTimestamp,
}

impl Lease {
    /// Nuovo lease; rifiuta durate nulle, negative o oltre [`LEASE_MAX_MILLIS`].
    pub fn new(
        session_id: SessionId,
        renewed_at: UtcTimestamp,
        duration_millis: i64,
    ) -> Result<Self, PublicError> {
        if !(1..=LEASE_MAX_MILLIS).contains(&duration_millis) {
            return Err(PublicError::invalid_argument(
                "durata del lease fuori da 1..=10000 ms",
            ));
        }
        let expires_at = renewed_at
            .checked_add_millis(duration_millis)
            .ok_or_else(|| PublicError::invalid_argument("istante del lease fuori intervallo"))?;
        Ok(Self {
            session_id,
            renewed_at,
            expires_at,
        })
    }

    /// `true` se il lease è valido all'istante `now` (scadenza esclusa).
    #[must_use]
    pub fn is_valid_at(&self, now: UtcTimestamp) -> bool {
        now < self.expires_at && self.expires_at.millis_since(self.renewed_at) <= LEASE_MAX_MILLIS
    }
}

/// Stato di una sessione agente.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SessionState {
    /// Attiva.
    Active,
    /// Pausa agente: nessuna nuova mutazione inoltrata; osservazione consentita.
    Paused,
    /// Terminata (revoca, scadenza, Emergency Stop, perdita del client...). Definitivo.
    Terminated,
}

/// Causa di terminazione di una sessione (registrata nel report, ROADMAP 9).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TerminationCause {
    /// Chiusura ordinaria dall'app.
    Completed,
    /// Revoca dell'utente.
    Revoked,
    /// Emergency Stop.
    EmergencyStop,
    /// Scadenza del grant.
    GrantExpired,
    /// Lease non rinnovato entro il limite.
    LeaseExpired,
    /// Perdita del client o dell'adapter.
    ClientLost,
    /// Chiusura o crash della UI.
    UiClosed,
    /// Perdita o riavvio del supervisor.
    SupervisorLost,
    /// Ripresa dell'host da sospensione.
    HostResumed,
    /// VM fallita o terminata durante la sessione.
    VmTerminated,
    /// Ripristino di baseline: la sessione di controllo si chiude (ROADMAP 8).
    Restored,
}

/// Esito di un test (ROADMAP 9). Il completamento della conversazione non produce
/// automaticamente `passed`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TestOutcome {
    /// Criteri verificati.
    Passed,
    /// Criteri non soddisfatti.
    Failed,
    /// Nessun verificatore affidabile o evidenze insufficienti.
    Inconclusive,
    /// Annullato dall'utente.
    Cancelled,
    /// Tempo esaurito.
    TimedOut,
    /// Guasto dell'infrastruttura, distinto dal fallimento del modello.
    InfrastructureError,
}

/// Rapporto di una revoca o di un Emergency Stop: distingue la coda scartata
/// dalle richieste già inoltrate (ROADMAP 7.3). Non promette annullamenti.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RevocationReport {
    /// Operazioni in coda scartate senza inoltro (nessun effetto).
    pub discarded: Vec<OperationId>,
    /// Operazioni già inoltrate al backend: possono aver prodotto effetti.
    pub already_forwarded: Vec<OperationId>,
}

/// Segreto di sessione (32 byte) emesso dal supervisor. Non è un identificativo:
/// non compare in log, report, argomenti di processo né in `Debug`.
///
/// In JSON (solo sul canale IPC autenticato) è una stringa di 64 cifre esadecimali.
#[derive(Clone, PartialEq, Eq)]
pub struct SessionToken([u8; 32]);

impl SessionToken {
    /// Token dai 32 byte generati dal supervisor con il generatore dell'OS.
    #[must_use]
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// Confronto a tempo costante rispetto al contenuto.
    #[must_use]
    pub fn ct_eq(&self, other: &SessionToken) -> bool {
        let mut diff = 0u8;
        for (a, b) in self.0.iter().zip(other.0.iter()) {
            diff |= a ^ b;
        }
        diff == 0
    }

    /// Byte del token (solo per l'archivio credenziali OS e il canale IPC).
    #[must_use]
    pub const fn expose_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl fmt::Debug for SessionToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SessionToken(<redatto>)")
    }
}

impl Serialize for SessionToken {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&crate::Sha256Digest::from_bytes(self.0).to_hex())
    }
}

impl<'de> Deserialize<'de> for SessionToken {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = Cow::<'de, str>::deserialize(deserializer)?;
        crate::Sha256Digest::parse_hex(&s)
            .map(|d| Self(*d.as_bytes()))
            .map_err(|_| serde::de::Error::custom("token di sessione non valido"))
    }
}

impl JsonSchema for SessionToken {
    fn schema_name() -> Cow<'static, str> {
        Cow::Borrowed("SessionToken")
    }
    fn json_schema(_generator: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "type": "string",
            "pattern": "^[0-9a-f]{64}$",
            "writeOnly": true,
            "description": "Segreto di sessione, solo su IPC autenticato; mai in log o report."
        })
    }
}

//! Involucro del protocollo IPC locale tra i client (UI, adapter MCP) e
//! `lightvm-service` (ROADMAP 6.1, 6.3, 7.3, 9; ADR-0002).
//!
//! Il trasporto (named pipe con ACL su Windows, socket Unix su Linux), il
//! framing (4 byte big-endian di lunghezza più JSON) e l'autenticazione del
//! canale appartengono al servizio; qui stanno soltanto i messaggi, con schema
//! JSON generato dalla stessa fonte ([`schemas`], file `schema/ipc_*.json`).
//!
//! Regole:
//! - il primo messaggio del client è [`ClientMessage::Hello`] con le versioni
//!   offerte e la credenziale del canale; il servizio sceglie la prima versione
//!   supportata in ordine di preferenza ([`SUPPORTED_PROTOCOL_VERSIONS`]);
//! - ogni richiesta porta un [`RequestId`] scelto dal client e restituito nella
//!   risposta; operazioni sconosciute non sono deserializzabili
//!   (`malformed_message`, nessun effetto);
//! - nessun campo è interpretato come percorso o opzione host: le VM sono
//!   riferite per [`VmId`], i media per [`crate::MediaId`] del registro;
//! - gli errori sono [`ReplyError`]: operativi ([`crate::PublicError`]) oppure
//!   di protocollo ([`crate::ProtocolError`]), mai confusi. Credenziale assente,
//!   malformata, sconosciuta o revocata → `unauthenticated`; ruolo insufficiente
//!   → `permission_denied` (vedi [`crate::ErrorCode`]).

use schemars::{JsonSchema, Schema, generate::SchemaSettings};
use serde::{Deserialize, Serialize};

use crate::{
    Admission, OperationRecord, PROTOCOL_VERSION, ReplyError, RunId, VmConfig, VmId, VmName,
    VmRecord, VmRequest, VmState,
};

/// Versioni del protocollo IPC supportate dal servizio, in ordine di preferenza.
pub const SUPPORTED_PROTOCOL_VERSIONS: &[u32] = &[PROTOCOL_VERSION];

/// Identificativo di una richiesta scelto dal client e restituito nella risposta.
pub type RequestId = u64;

/// Identificativo di una connessione assegnato dal servizio all'handshake.
pub type ConnectionId = u64;

/// Ruolo associato alla credenziale del canale.
///
/// `ui` corrisponde a [`crate::Caller::User`] verso il core; `pairing` non
/// conferisce permessi sulle VM (ADR-0006): il grant di sessione agente arriva
/// in F4 ([`crate::Grant`], [`crate::SessionToken`]).
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    /// Interfaccia desktop che ha avviato il servizio.
    Ui,
    /// Client in attesa di pairing (adapter MCP): nessun permesso VM.
    Pairing,
}

/// Messaggi dal client al servizio.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMessage {
    /// Primo messaggio obbligatorio.
    Hello {
        /// Identificativo della richiesta.
        request_id: RequestId,
        /// Versioni del protocollo offerte dal client.
        protocol_versions: Vec<u32>,
        /// Credenziale del canale: 32 byte in 64 cifre esadecimali minuscole.
        /// Resta testo libero nello schema perché una credenziale malformata
        /// dia lo stesso esito (`unauthenticated`) di una assente o errata,
        /// senza distinguere i motivi. Segreto: mai in log o report.
        #[serde(default)]
        #[schemars(extend("writeOnly" = true))]
        credential: Option<String>,
        /// Nome del client, solo diagnostico (troncato a 64 caratteri).
        #[serde(default)]
        client_name: Option<String>,
    },
    /// Richiesta tipizzata dopo l'handshake.
    Request {
        /// Identificativo della richiesta.
        request_id: RequestId,
        /// Operazione richiesta.
        body: Request,
    },
}

/// Operazioni tipizzate.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Request {
    /// Verifica di vivacità.
    Ping,
    /// Capacità del servizio e del backend.
    GetCapabilities,
    /// Elenco delle VM del catalogo (solo UI).
    ListVms,
    /// Stato di una VM (solo UI).
    VmStatus {
        /// VM.
        vm_id: VmId,
    },
    /// Creazione di una VM nel catalogo (solo UI; mai da sessioni agente).
    CreateVm {
        /// Configurazione richiesta.
        config: VmConfig,
    },
    /// Modifica della configurazione di una VM spenta: risorse e limiti
    /// (solo UI; il disco si può solo ingrandire).
    UpdateVm {
        /// VM.
        vm_id: VmId,
        /// Revisione attesa: una modifica concorrente è rifiutata.
        expected_revision: u64,
        /// Nuova configurazione completa.
        config: VmConfig,
    },
    /// Azione su una VM esistente tramite la coda del core (solo UI in F1).
    /// La risposta arriva dopo l'esito del backend.
    SubmitVm {
        /// Richiesta con chiave di idempotenza.
        request: VmRequest,
    },
    /// Iscrizione agli eventi di stato (solo UI).
    Subscribe,
    /// Elenco delle connessioni attive (solo UI).
    ListConnections,
    /// Revoca di una connessione e della sua credenziale (solo UI).
    RevokeConnection {
        /// Connessione da revocare.
        connection_id: ConnectionId,
    },
    /// Emissione di una credenziale di pairing da consegnare fuori banda (solo UI).
    CreatePairingCredential,
    /// Emergency Stop: percorso prioritario, non passa dalla coda (solo UI).
    EmergencyStop,
    /// Sorte delle VM se la UI termina senza arresto controllato (solo UI).
    /// Vale subito; la UI la ripassa a ogni avvio del servizio.
    SetParentLossPolicy {
        /// Nuova politica.
        policy: ParentLossPolicy,
    },
    /// Arresto controllato del servizio (solo UI).
    Shutdown,
}

impl Request {
    /// Nome stabile dell'operazione, per audit e diagnostica.
    #[must_use]
    pub const fn name(&self) -> &'static str {
        match self {
            Self::Ping => "ping",
            Self::GetCapabilities => "get_capabilities",
            Self::ListVms => "list_vms",
            Self::VmStatus { .. } => "vm_status",
            Self::CreateVm { .. } => "create_vm",
            Self::UpdateVm { .. } => "update_vm",
            Self::SubmitVm { .. } => "submit_vm",
            Self::Subscribe => "subscribe",
            Self::ListConnections => "list_connections",
            Self::RevokeConnection { .. } => "revoke_connection",
            Self::CreatePairingCredential => "create_pairing_credential",
            Self::EmergencyStop => "emergency_stop",
            Self::SetParentLossPolicy { .. } => "set_parent_loss_policy",
            Self::Shutdown => "shutdown",
        }
    }
}

/// Riepilogo di una VM per elenco e stato.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct VmSummary {
    /// VM.
    pub vm_id: VmId,
    /// Nome mostrato.
    pub name: VmName,
    /// Stato corrente come visto dal core (derivato da processo e QMP).
    pub state: VmState,
    /// Esecuzione corrente, se esiste un processo o l'ultima conclusa.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<RunId>,
    /// Configurazione corrente (risorse e limiti), per le impostazioni della VM.
    pub config: VmConfig,
    /// Revisione della configurazione, da indicare in `update_vm`.
    pub config_revision: u64,
}

/// Capacità riportate da `get_capabilities`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Capabilities {
    /// Versioni del protocollo supportate.
    pub protocol_versions: Vec<u32>,
    /// Nome del backend.
    pub backend: String,
    /// Acceleratore disponibile, se rilevato.
    pub accelerator: Option<String>,
    /// Limite di byte per frame.
    pub max_frame_bytes: u32,
    /// Politica attiva alla perdita della UI.
    #[serde(default)]
    pub parent_loss_policy: ParentLossPolicy,
}

/// Attesa minima ammessa per lo spegnimento ordinato alla perdita della UI.
pub const MIN_PARENT_LOSS_POWERDOWN_S: u32 = 1;
/// Attesa massima ammessa per lo spegnimento ordinato alla perdita della UI.
pub const MAX_PARENT_LOSS_POWERDOWN_S: u32 = 300;

/// Sorte delle VM quando la UI che ha avviato il servizio termina senza
/// arresto controllato (crash, terminazione forzata). ADR-0015 parte B.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ParentLossPolicy {
    /// Emergency Stop di tutte le VM, poi arresto del servizio (predefinita).
    #[default]
    KillVms,
    /// Spegnimento ordinato (ACPI) con attesa limitata, poi `quit` e
    /// terminazione; infine arresto del servizio.
    ShutdownVms {
        /// Attesa dello spegnimento ACPI per VM, in secondi
        /// ([`MIN_PARENT_LOSS_POWERDOWN_S`]..=[`MAX_PARENT_LOSS_POWERDOWN_S`]).
        powerdown_s: u32,
    },
    /// Le VM restano accese e il servizio resta attivo senza UI.
    KeepVms,
}

impl ParentLossPolicy {
    /// Validazione dei limiti.
    pub fn validate(self) -> Result<Self, crate::PublicError> {
        match self {
            Self::ShutdownVms { powerdown_s }
                if !(MIN_PARENT_LOSS_POWERDOWN_S..=MAX_PARENT_LOSS_POWERDOWN_S)
                    .contains(&powerdown_s) =>
            {
                Err(crate::PublicError::invalid_argument(format!(
                    "powerdown_s fuori dall'intervallo {MIN_PARENT_LOSS_POWERDOWN_S}..={MAX_PARENT_LOSS_POWERDOWN_S}"
                )))
            }
            p => Ok(p),
        }
    }
}

/// Connessione attiva, per `list_connections`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ConnectionInfo {
    /// Identificativo della connessione.
    pub connection_id: ConnectionId,
    /// Ruolo della credenziale.
    pub role: Role,
    /// PID del peer riportato dal sistema operativo, solo diagnostico.
    pub peer_pid: Option<u32>,
    /// Nome dichiarato dal client, solo diagnostico.
    pub client_name: Option<String>,
}

/// Esito dell'Emergency Stop riportato al client.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct EmergencyStopReport {
    /// Connessioni non UI revocate.
    pub revoked_connections: u32,
    /// Processi di cui il backend ha osservato la fine.
    pub processes_terminated: u32,
    /// Vero solo se la fine di tutti i processi è stata osservata.
    pub completed: bool,
}

/// Risultati delle operazioni.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ResponseBody {
    /// Risposta a `ping`.
    Pong,
    /// Risposta a `get_capabilities`.
    Capabilities(Capabilities),
    /// Risposta a `list_vms`.
    VmList {
        /// VM visibili.
        vms: Vec<VmSummary>,
    },
    /// Risposta a `vm_status`.
    Vm {
        /// Stato della VM.
        vm: VmSummary,
    },
    /// Risposta a `create_vm`.
    VmCreated {
        /// VM creata, con identificativo assegnato dal core.
        vm: VmRecord,
    },
    /// Risposta a `update_vm`: VM con la nuova revisione.
    VmUpdated {
        /// VM aggiornata.
        vm: VmRecord,
    },
    /// Risposta a `submit_vm`: accettazione ed esito registrato.
    Operation {
        /// Accettazione (o duplicato idempotente).
        admission: Admission,
        /// Registro dell'operazione dopo l'esecuzione, se disponibile.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        operation: Option<OperationRecord>,
    },
    /// Iscrizione accettata.
    Subscribed,
    /// Risposta a `list_connections`.
    Connections {
        /// Connessioni attive.
        connections: Vec<ConnectionInfo>,
    },
    /// Connessione revocata.
    Revoked {
        /// Connessione revocata.
        connection_id: ConnectionId,
    },
    /// Credenziale di pairing emessa: va consegnata fuori banda, mai registrata.
    PairingCredential {
        /// Credenziale in esadecimale.
        #[schemars(extend("writeOnly" = true))]
        credential: String,
        /// Scadenza in secondi.
        expires_in_s: u64,
    },
    /// Esito dell'Emergency Stop.
    EmergencyStop(EmergencyStopReport),
    /// Politica alla perdita della UI in vigore dopo `set_parent_loss_policy`.
    ParentLossPolicy {
        /// Politica attiva.
        policy: ParentLossPolicy,
    },
    /// Arresto accettato.
    ShuttingDown,
}

/// Stato del servizio.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum ServiceState {
    /// In servizio.
    Running,
    /// Arresto controllato in corso.
    Stopping,
}

/// Motivo dell'arresto delle sessioni agente.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    /// Processo genitore (UI) terminato.
    ParentLost,
    /// Emergency Stop.
    EmergencyStop,
    /// Arresto controllato del servizio.
    Shutdown,
}

/// Eventi pubblicati sul canale di stato.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ServiceEvent {
    /// Cambio di stato del servizio.
    ServiceState {
        /// Nuovo stato.
        state: ServiceState,
    },
    /// Cambio di stato di una VM.
    VmState {
        /// VM.
        vm_id: VmId,
        /// Nuovo stato.
        state: VmState,
    },
    /// Una connessione è stata revocata.
    ConnectionRevoked {
        /// Connessione revocata.
        connection_id: ConnectionId,
    },
    /// Le sessioni agente sono state arrestate.
    AgentSessionsStopped {
        /// Motivo.
        reason: StopReason,
    },
    /// Eventi persi per lentezza del client.
    EventsLost {
        /// Numero di eventi persi.
        count: u64,
    },
}

/// Motivo di chiusura della connessione da parte del servizio.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum CloseReason {
    /// Connessione o credenziale revocata.
    Revoked,
    /// Arresto del servizio.
    ServiceStopping,
}

/// Messaggi dal servizio al client.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerMessage {
    /// Handshake accettato.
    HelloOk {
        /// Richiesta di `hello`.
        request_id: RequestId,
        /// Versione negoziata.
        protocol_version: u32,
        /// Identificativo assegnato alla connessione.
        connection_id: ConnectionId,
        /// Ruolo della credenziale.
        role: Role,
    },
    /// Risposta positiva.
    Response {
        /// Richiesta.
        request_id: RequestId,
        /// Risultato.
        result: ResponseBody,
    },
    /// Errore; `request_id` assente se il frame non era decodificabile.
    Error {
        /// Richiesta, se nota.
        request_id: Option<RequestId>,
        /// Errore.
        error: ReplyError,
    },
    /// Evento di stato (dopo `subscribe`).
    Event {
        /// Evento.
        event: ServiceEvent,
    },
    /// Chiusura avviata dal servizio; segue la chiusura del trasporto.
    Closing {
        /// Motivo.
        reason: CloseReason,
    },
}

fn generate<T: JsonSchema>() -> Schema {
    SchemaSettings::draft2020_12()
        .into_generator()
        .into_root_schema_for::<T>()
}

/// Schemi dell'involucro IPC (nome file senza estensione, schema), in ordine
/// stabile. I due messaggi radice includono in `$defs` tutti i tipi annidati.
#[must_use]
pub fn schemas() -> Vec<(&'static str, Schema)> {
    vec![
        ("ipc_client_message", generate::<ClientMessage>()),
        ("ipc_server_message", generate::<ServerMessage>()),
        ("parent_loss_policy", generate::<ParentLossPolicy>()),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ErrorCode, ProtocolError, ProtocolErrorCode, PublicError};

    #[test]
    fn parent_loss_policy_wire_shape_default_and_limits() {
        assert_eq!(ParentLossPolicy::default(), ParentLossPolicy::KillVms);
        assert_eq!(
            serde_json::to_string(&ParentLossPolicy::KillVms).unwrap(),
            r#"{"kind":"kill_vms"}"#
        );
        assert_eq!(
            serde_json::to_string(&ParentLossPolicy::ShutdownVms { powerdown_s: 10 }).unwrap(),
            r#"{"kind":"shutdown_vms","powerdown_s":10}"#
        );
        assert_eq!(
            serde_json::to_string(&ParentLossPolicy::KeepVms).unwrap(),
            r#"{"kind":"keep_vms"}"#
        );
        for ok in [1, 10, 300] {
            assert!(
                ParentLossPolicy::ShutdownVms { powerdown_s: ok }
                    .validate()
                    .is_ok()
            );
        }
        for bad in [0, 301, u32::MAX] {
            let e = ParentLossPolicy::ShutdownVms { powerdown_s: bad }
                .validate()
                .unwrap_err();
            assert_eq!(e.code, ErrorCode::InvalidArgument);
        }
        // Capacità di un servizio precedente senza il campo: politica predefinita.
        let c: Capabilities = serde_json::from_str(
            r#"{"protocol_versions":[1],"backend":"x","accelerator":null,"max_frame_bytes":1}"#,
        )
        .unwrap();
        assert_eq!(c.parent_loss_policy, ParentLossPolicy::KillVms);
    }

    #[test]
    fn error_json_shape_uses_proto_types() {
        let e = ServerMessage::Error {
            request_id: Some(7),
            error: ReplyError::Operational(PublicError::unauthenticated("x")),
        };
        assert_eq!(
            serde_json::to_string(&e).unwrap(),
            r#"{"type":"error","request_id":7,"error":{"kind":"operational","code":"unauthenticated","message":"x","retryable":false}}"#
        );
        let p = ServerMessage::Error {
            request_id: None,
            error: ReplyError::Protocol(ProtocolError {
                code: ProtocolErrorCode::MessageTooLarge,
                message: "y".into(),
            }),
        };
        assert_eq!(
            serde_json::to_string(&p).unwrap(),
            r#"{"type":"error","request_id":null,"error":{"kind":"protocol","code":"message_too_large","message":"y"}}"#
        );
        let d = ReplyError::Operational(PublicError::new(ErrorCode::PermissionDenied, "z"));
        assert!(
            serde_json::to_string(&d)
                .unwrap()
                .contains("permission_denied")
        );
    }

    #[test]
    fn request_json_shape() {
        let m: ClientMessage =
            serde_json::from_str(r#"{"type":"request","request_id":3,"body":{"op":"ping"}}"#)
                .unwrap();
        assert_eq!(
            m,
            ClientMessage::Request {
                request_id: 3,
                body: Request::Ping
            }
        );
        let vm = VmId::from_unix_millis(1_760_000_000_000, [0; 10]);
        let m = ClientMessage::Request {
            request_id: 4,
            body: Request::VmStatus { vm_id: vm },
        };
        let s = serde_json::to_string(&m).unwrap();
        assert_eq!(serde_json::from_str::<ClientMessage>(&s).unwrap(), m);
    }

    #[test]
    fn unknown_ops_are_rejected() {
        assert!(
            serde_json::from_str::<ClientMessage>(
                r#"{"type":"request","request_id":3,"body":{"op":"qmp_execute"}}"#
            )
            .is_err()
        );
    }

    #[test]
    fn credential_is_write_only_in_schema() {
        let s = serde_json::to_string(&generate::<ClientMessage>()).unwrap();
        assert!(s.contains("writeOnly"));
    }
}

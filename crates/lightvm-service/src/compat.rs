//! Tipi di protocollo IPC del servizio.
//!
//! Gli errori vengono da `lightvm-proto` (fonte unica, ROADMAP 6.3):
//! [`PublicError`]/[`ErrorCode`] per gli errori operativi e [`ProtocolError`]
//! per quelli di protocollo, uniti in [`ReplyError`]. L'involucro dei messaggi
//! (hello, richieste, risposte, eventi) e il ruolo della credenziale restano
//! locali.
//!
//! TODO(F1): spostare l'involucro IPC in `lightvm-proto` (con JSON Schema
//! generato) e allineare [`Role`] con `Caller`/`Grant` di `lightvm-proto`
//! quando il grant di sessione entra in uso (F4).

pub use lightvm_proto::{ErrorCode, ProtocolError, ProtocolErrorCode, PublicError, ReplyError};
use serde::{Deserialize, Serialize};

/// Versioni del protocollo IPC supportate dal servizio, in ordine di preferenza.
pub const SUPPORTED_PROTOCOL_VERSIONS: &[u32] = &[1];

/// Errore operativo pubblico con codice stabile.
pub fn op_error(code: ErrorCode, message: impl Into<String>) -> ReplyError {
    ReplyError::Operational(PublicError::new(code, message))
}

/// Errore di protocollo: nessun effetto tentato.
pub fn protocol_error(code: ProtocolErrorCode, message: impl Into<String>) -> ReplyError {
    ReplyError::Protocol(ProtocolError {
        code,
        message: message.into(),
    })
}

/// Codice operativo di un errore di risposta, se lo è.
pub fn operational_code(e: &ReplyError) -> Option<ErrorCode> {
    match e {
        ReplyError::Operational(p) => Some(p.code),
        ReplyError::Protocol(_) => None,
    }
}

/// Codice di protocollo di un errore di risposta, se lo è.
pub fn protocol_code(e: &ReplyError) -> Option<ProtocolErrorCode> {
    match e {
        ReplyError::Operational(_) => None,
        ReplyError::Protocol(p) => Some(p.code),
    }
}

/// Identificativo di una richiesta scelto dal client e restituito nella risposta.
pub type RequestId = u64;

/// Ruolo associato a una credenziale.
///
/// TODO(F1): allineare con `lightvm_proto::Grant` quando disponibile. `Pairing`
/// non conferisce permessi sulle VM (ADR-0006); il grant di sessione arriva in F4.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    /// Interfaccia desktop che ha avviato il servizio.
    Ui,
    /// Client in attesa di pairing (adapter MCP): nessun permesso VM.
    Pairing,
}

/// Messaggi dal client al servizio.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMessage {
    /// Primo messaggio obbligatorio.
    Hello {
        /// Identificativo della richiesta.
        request_id: RequestId,
        /// Versioni del protocollo offerte dal client.
        protocol_versions: Vec<u32>,
        /// Credenziale (32 byte in esadecimale). Assente → `permission_denied`.
        #[serde(default)]
        credential: Option<String>,
        /// Nome del client, solo diagnostico.
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

/// Operazioni tipizzate. Nessun campo è interpretato come percorso o opzione host.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Request {
    /// Verifica di vivacità.
    Ping,
    /// Capacità del servizio e del backend.
    GetCapabilities,
    /// Elenco delle VM del catalogo.
    ListVms,
    /// Iscrizione agli eventi di stato.
    Subscribe,
    /// Elenco delle connessioni attive (solo UI).
    ListConnections,
    /// Revoca di una connessione e della sua credenziale (solo UI).
    RevokeConnection {
        /// Connessione da revocare.
        connection_id: u64,
    },
    /// Emissione di una credenziale di pairing da consegnare fuori banda (solo UI).
    CreatePairingCredential,
    /// Emergency Stop: percorso prioritario, non passa dalla coda (solo UI).
    EmergencyStop,
    /// Arresto controllato del servizio (solo UI).
    Shutdown,
}

impl Request {
    /// Nome stabile dell'operazione, per audit e diagnostica.
    pub fn name(&self) -> &'static str {
        match self {
            Self::Ping => "ping",
            Self::GetCapabilities => "get_capabilities",
            Self::ListVms => "list_vms",
            Self::Subscribe => "subscribe",
            Self::ListConnections => "list_connections",
            Self::RevokeConnection { .. } => "revoke_connection",
            Self::CreatePairingCredential => "create_pairing_credential",
            Self::EmergencyStop => "emergency_stop",
            Self::Shutdown => "shutdown",
        }
    }
}

/// Riepilogo di una VM per l'elenco.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VmSummary {
    /// Identificativo opaco della VM.
    pub vm_id: String,
    /// Nome mostrato.
    pub name: String,
    /// Stato osservato.
    pub state: VmState,
}

/// Stato osservato della VM (ARCHITECTURE 4).
///
/// TODO(F1): sostituire con il tipo di `vm-core`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VmState {
    /// Processo assente.
    Stopped,
    /// Avvio in corso.
    Starting,
    /// Processo vivo e QMP pronto.
    Running,
    /// Arresto in corso.
    Stopping,
    /// Errore di avvio o uscita inattesa.
    Failed,
    /// Riconciliazione dopo un crash.
    Recovering,
}

/// Capacità riportate da `get_capabilities`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Capabilities {
    /// Versioni del protocollo supportate.
    pub protocol_versions: Vec<u32>,
    /// Nome del backend.
    pub backend: String,
    /// Acceleratore disponibile, se rilevato.
    pub accelerator: Option<String>,
    /// Limite di byte per frame.
    pub max_frame_bytes: u32,
}

/// Connessione attiva, per `list_connections`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConnectionInfo {
    /// Identificativo della connessione.
    pub connection_id: u64,
    /// Ruolo della credenziale.
    pub role: Role,
    /// PID del peer riportato dal sistema operativo, solo diagnostico.
    pub peer_pid: Option<u32>,
    /// Nome dichiarato dal client, solo diagnostico.
    pub client_name: Option<String>,
}

/// Esito dell'Emergency Stop riportato al client.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmergencyStopReport {
    /// Connessioni non UI revocate.
    pub revoked_connections: u32,
    /// Processi di cui il backend ha osservato la fine.
    pub processes_terminated: u32,
    /// Vero solo se la fine di tutti i processi è stata osservata.
    pub completed: bool,
}

/// Risultati delle operazioni.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
        connection_id: u64,
    },
    /// Credenziale di pairing emessa: va consegnata fuori banda, mai registrata.
    PairingCredential {
        /// Credenziale in esadecimale.
        credential: String,
        /// Scadenza in secondi.
        expires_in_s: u64,
    },
    /// Esito dell'Emergency Stop.
    EmergencyStop(EmergencyStopReport),
    /// Arresto accettato.
    ShuttingDown,
}

/// Eventi pubblicati sul canale di stato.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
        vm_id: String,
        /// Nuovo stato.
        state: VmState,
    },
    /// Una connessione è stata revocata.
    ConnectionRevoked {
        /// Connessione revocata.
        connection_id: u64,
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

/// Stato del servizio.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ServiceState {
    /// In servizio.
    Running,
    /// Arresto controllato in corso.
    Stopping,
}

/// Motivo dell'arresto delle sessioni agente.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    /// Processo genitore (UI) terminato.
    ParentLost,
    /// Emergency Stop.
    EmergencyStop,
    /// Arresto controllato del servizio.
    Shutdown,
}

/// Messaggi dal servizio al client.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerMessage {
    /// Handshake accettato.
    HelloOk {
        /// Richiesta di `hello`.
        request_id: RequestId,
        /// Versione negoziata.
        protocol_version: u32,
        /// Identificativo assegnato alla connessione.
        connection_id: u64,
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

/// Motivo di chiusura della connessione da parte del servizio.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CloseReason {
    /// Connessione o credenziale revocata.
    Revoked,
    /// Arresto del servizio.
    ServiceStopping,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_json_shape_uses_proto_types() {
        let e = ServerMessage::Error {
            request_id: Some(7),
            error: op_error(ErrorCode::PermissionDenied, "x"),
        };
        let s = serde_json::to_string(&e).unwrap();
        assert_eq!(
            s,
            r#"{"type":"error","request_id":7,"error":{"kind":"operational","code":"permission_denied","message":"x","retryable":false}}"#
        );
        let p = ServerMessage::Error {
            request_id: None,
            error: protocol_error(ProtocolErrorCode::MessageTooLarge, "y"),
        };
        assert_eq!(
            serde_json::to_string(&p).unwrap(),
            r#"{"type":"error","request_id":null,"error":{"kind":"protocol","code":"message_too_large","message":"y"}}"#
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
    }

    #[test]
    fn unknown_ops_are_rejected_as_malformed() {
        assert!(
            serde_json::from_str::<ClientMessage>(
                r#"{"type":"request","request_id":3,"body":{"op":"qmp_execute"}}"#
            )
            .is_err()
        );
    }
}

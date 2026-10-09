//! Spike F1-19: prova dell'SDK Rust ufficiale MCP (`rmcp`) con trasporto stdio.
//!
//! Il server espone due tool **finti** (`vm_list`, `vm_status`) su un catalogo
//! statico in memoria: nessuna VM reale, nessun supervisor, nessun IPC, nessuna
//! rete. Serve solo a verificare che l'SDK scelto in ADR-0021:
//!
//! - negozi la revisione di protocollo 2025-11-25 e nessun'altra;
//! - pubblichi i tool con JSON Schema di ingresso e di uscita;
//! - restituisca risultati strutturati e distingua gli errori operativi
//!   (`isError: true` nel risultato del tool) dagli errori di protocollo
//!   (errore JSON-RPC);
//! - scriva su stdout soltanto messaggi MCP (diagnostica su stderr).
//!
//! L'adapter di prodotto (`mcp-server`, ROADMAP 6.2) nasce in F5 e si collega
//! al supervisor; questo crate non ne anticipa l'API.

use std::borrow::Cow;

use rmcp::{
    Json, ServerHandler,
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{Implementation, ProtocolVersion, ServerCapabilities, ServerConfig},
    tool, tool_handler, tool_router,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Unica revisione MCP accettata (ROADMAP sezione 9; ADR-0021).
pub const PROTOCOL_VERSION: ProtocolVersion = ProtocolVersion::V_2025_11_25;

/// Elenco delle revisioni supportate: soltanto la baseline.
const SUPPORTED_VERSIONS: &[ProtocolVersion] = &[ProtocolVersion::V_2025_11_25];

/// Nome del server annunciato in `serverInfo`.
pub const SERVER_NAME: &str = "lightvm-mcp-spike";

/// Stato finto di una VM nel catalogo statico dello spike.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum FakeVmState {
    /// VM spenta.
    Stopped,
    /// VM in esecuzione.
    Running,
}

/// Riepilogo di una VM restituito da `vm_list`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct VmSummary {
    /// Identificativo stabile della VM.
    pub vm_id: String,
    /// Nome leggibile.
    pub name: String,
    /// Stato corrente.
    pub state: FakeVmState,
}

/// Risultato strutturato di `vm_list`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct VmListResult {
    /// VM visibili alla sessione (catalogo finto).
    pub vms: Vec<VmSummary>,
}

/// Parametri di `vm_status`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct VmStatusParams {
    /// Identificativo della VM, come restituito da `vm_list`.
    #[schemars(length(min = 1, max = 64))]
    pub vm_id: String,
}

/// Risultato strutturato di `vm_status`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct VmStatusResult {
    /// Identificativo della VM.
    pub vm_id: String,
    /// Stato corrente.
    pub state: FakeVmState,
    /// Numero di vCPU configurate.
    pub vcpus: u32,
    /// Memoria configurata in MiB.
    pub memory_mib: u32,
    /// Sempre `true`: i dati vengono da un catalogo finto.
    pub simulated: bool,
}

/// Catalogo statico dello spike: dati finti dichiarati come tali.
fn fake_catalog() -> Vec<VmStatusResult> {
    vec![
        VmStatusResult {
            vm_id: "vm-demo-1".to_owned(),
            state: FakeVmState::Stopped,
            vcpus: 2,
            memory_mib: 4096,
            simulated: true,
        },
        VmStatusResult {
            vm_id: "vm-demo-2".to_owned(),
            state: FakeVmState::Running,
            vcpus: 1,
            memory_mib: 2048,
            simulated: true,
        },
    ]
}

/// Server MCP dello spike.
#[derive(Debug, Clone)]
pub struct SpikeServer {
    tool_router: ToolRouter<Self>,
}

impl Default for SpikeServer {
    fn default() -> Self {
        Self::new()
    }
}

#[tool_router(router = tool_router)]
impl SpikeServer {
    /// Crea il server con il router dei tool.
    pub fn new() -> Self {
        Self {
            tool_router: Self::tool_router(),
        }
    }

    /// Elenca le VM del catalogo finto.
    #[tool(
        name = "vm_list",
        description = "Elenca le VM visibili (catalogo finto dello spike, nessuna VM reale)."
    )]
    pub async fn vm_list(&self) -> Json<VmListResult> {
        let vms = fake_catalog()
            .into_iter()
            .enumerate()
            .map(|(i, vm)| VmSummary {
                name: format!("Demo {}", i + 1),
                vm_id: vm.vm_id,
                state: vm.state,
            })
            .collect();
        Json(VmListResult { vms })
    }

    /// Restituisce lo stato di una VM del catalogo finto.
    ///
    /// Una VM sconosciuta è un errore operativo: risultato con `isError: true`,
    /// non un errore JSON-RPC (ROADMAP sezione 9, "errori operativi distinti da
    /// errori del protocollo").
    #[tool(
        name = "vm_status",
        description = "Stato di una VM del catalogo finto dello spike."
    )]
    pub async fn vm_status(
        &self,
        Parameters(params): Parameters<VmStatusParams>,
    ) -> Result<Json<VmStatusResult>, String> {
        fake_catalog()
            .into_iter()
            .find(|vm| vm.vm_id == params.vm_id)
            .map(Json)
            .ok_or_else(|| "vm_not_found: nessuna VM con questo identificativo".to_owned())
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for SpikeServer {
    fn get_info(&self) -> ServerConfig {
        let mut info = ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new(SERVER_NAME, env!("CARGO_PKG_VERSION")))
            .with_instructions("Spike LightVM: tool finti, nessuna VM reale.");
        info.protocol_version = PROTOCOL_VERSION;
        info
    }

    fn supported_protocol_versions(&self) -> Cow<'static, [ProtocolVersion]> {
        Cow::Borrowed(SUPPORTED_VERSIONS)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn solo_la_baseline_e_supportata() {
        let server = SpikeServer::new();
        let versions = server.supported_protocol_versions();
        assert_eq!(versions.as_ref(), &[ProtocolVersion::V_2025_11_25]);
        assert_eq!(server.get_info().protocol_version.as_str(), "2025-11-25");
    }

    #[test]
    fn catalogo_dichiarato_simulato() {
        assert!(fake_catalog().iter().all(|vm| vm.simulated));
    }
}

//! Spike F1-19: trial of the official MCP Rust SDK (`rmcp`) with stdio transport.
//!
//! The server exposes two **fake** tools (`vm_list`, `vm_status`) over a static
//! in-memory catalog: no real VM, no supervisor, no IPC, no network. It only
//! serves to verify that the SDK chosen in ADR-0021:
//!
//! - negotiates protocol revision 2025-11-25 and no other;
//! - publishes the tools with input and output JSON Schema;
//! - returns structured results and distinguishes operational errors
//!   (`isError: true` in the tool result) from protocol errors (JSON-RPC
//!   error);
//! - writes only MCP messages to stdout (diagnostics on stderr).
//!
//! The product adapter (`mcp-server`, ROADMAP 6.2) is born in F5 and connects
//! to the supervisor; this crate does not anticipate its API.

use std::borrow::Cow;

use rmcp::{
    Json, ServerHandler,
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{Implementation, ProtocolVersion, ServerCapabilities, ServerConfig},
    tool, tool_handler, tool_router,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Only accepted MCP revision (ROADMAP section 9; ADR-0021).
pub const PROTOCOL_VERSION: ProtocolVersion = ProtocolVersion::V_2025_11_25;

/// List of supported revisions: only the baseline.
const SUPPORTED_VERSIONS: &[ProtocolVersion] = &[ProtocolVersion::V_2025_11_25];

/// Server name announced in `serverInfo`.
pub const SERVER_NAME: &str = "lightvm-mcp-spike";

/// Fake state of a VM in the spike's static catalog.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum FakeVmState {
    /// VM powered off.
    Stopped,
    /// VM running.
    Running,
}

/// Summary of a VM returned by `vm_list`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct VmSummary {
    /// Stable identifier of the VM.
    pub vm_id: String,
    /// Human-readable name.
    pub name: String,
    /// Current state.
    pub state: FakeVmState,
}

/// Structured result of `vm_list`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct VmListResult {
    /// VMs visible to the session (fake catalog).
    pub vms: Vec<VmSummary>,
}

/// Parameters of `vm_status`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct VmStatusParams {
    /// Identifier of the VM, as returned by `vm_list`.
    #[schemars(length(min = 1, max = 64))]
    pub vm_id: String,
}

/// Structured result of `vm_status`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct VmStatusResult {
    /// Identifier of the VM.
    pub vm_id: String,
    /// Current state.
    pub state: FakeVmState,
    /// Number of configured vCPUs.
    pub vcpus: u32,
    /// Configured memory in MiB.
    pub memory_mib: u32,
    /// Always `true`: the data come from a fake catalog.
    pub simulated: bool,
}

/// Static catalog of the spike: fake data declared as such.
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

/// MCP server of the spike.
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
    /// Creates the server with the tool router.
    pub fn new() -> Self {
        Self {
            tool_router: Self::tool_router(),
        }
    }

    /// Lists the VMs of the fake catalog.
    #[tool(
        name = "vm_list",
        description = "Lists the visible VMs (fake spike catalog, no real VM)."
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

    /// Returns the state of a VM of the fake catalog.
    ///
    /// An unknown VM is an operational error: a result with `isError: true`,
    /// not a JSON-RPC error (ROADMAP section 9, "operational errors distinct
    /// from protocol errors").
    #[tool(
        name = "vm_status",
        description = "State of a VM of the fake spike catalog."
    )]
    pub async fn vm_status(
        &self,
        Parameters(params): Parameters<VmStatusParams>,
    ) -> Result<Json<VmStatusResult>, String> {
        fake_catalog()
            .into_iter()
            .find(|vm| vm.vm_id == params.vm_id)
            .map(Json)
            .ok_or_else(|| "vm_not_found: no VM with this identifier".to_owned())
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for SpikeServer {
    fn get_info(&self) -> ServerConfig {
        let mut info = ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new(SERVER_NAME, env!("CARGO_PKG_VERSION")))
            .with_instructions("LightVM spike: fake tools, no real VM.");
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
    fn only_the_baseline_is_supported() {
        let server = SpikeServer::new();
        let versions = server.supported_protocol_versions();
        assert_eq!(versions.as_ref(), &[ProtocolVersion::V_2025_11_25]);
        assert_eq!(server.get_info().protocol_version.as_str(), "2025-11-25");
    }

    #[test]
    fn catalog_declared_simulated() {
        assert!(fake_catalog().iter().all(|vm| vm.simulated));
    }
}

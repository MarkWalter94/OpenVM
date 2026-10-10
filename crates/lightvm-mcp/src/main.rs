//! MCP spike binary: stdio server with fake tools (F1-19, ADR-0021).
//!
//! stdout carries only MCP messages (newline-delimited JSON-RPC); diagnostics
//! go to stderr and contain no secrets (ROADMAP section 9).

use lightvm_mcp::SpikeServer;
use rmcp::{ServiceExt, transport::stdio};

#[tokio::main(flavor = "current_thread")]
async fn main() -> std::process::ExitCode {
    eprintln!("lightvm-mcp-spike: starting on stdio (fake tools, no real VM)");
    let running = match SpikeServer::new().serve(stdio()).await {
        Ok(running) => running,
        Err(err) => {
            eprintln!("lightvm-mcp-spike: initialization failed: {err}");
            return std::process::ExitCode::from(2);
        }
    };
    match running.waiting().await {
        Ok(reason) => {
            eprintln!("lightvm-mcp-spike: closing ({reason:?})");
            std::process::ExitCode::SUCCESS
        }
        Err(err) => {
            eprintln!("lightvm-mcp-spike: service error: {err}");
            std::process::ExitCode::from(1)
        }
    }
}

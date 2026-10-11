//! `lightvm-mcp`: MCP stdio adapter of LightVM (F5, ADR-0006, ADR-0021).
//!
//! Usage: `lightvm-mcp` (no argument). The pairing file written by the app
//! is read from its default location, or from the file named by
//! `LIGHTVM_PAIRING_FILE`; the credential is never an argument.
//!
//! stdout carries only MCP messages (newline-delimited JSON-RPC);
//! diagnostics go to stderr and contain no secrets.

use std::process::ExitCode;

use lightvm_mcp::{Adapter, AdapterConfig, pairing};
use rmcp::{ServiceExt, transport::stdio};

const USAGE: &str = "usage: lightvm-mcp | --version | --help\n\
Pair a client in LightVM -> Agents first: the app writes the pairing file the adapter reads\n\
(default location, or the file named by LIGHTVM_PAIRING_FILE). MCP over stdio, revision 2025-11-25.";

fn main() -> ExitCode {
    if let Some(a) = std::env::args_os().nth(1) {
        match a.to_str() {
            Some("--version") => {
                eprintln!("lightvm-mcp {}", env!("CARGO_PKG_VERSION"));
                return ExitCode::SUCCESS;
            }
            Some("--help" | "-h") => {
                eprintln!("{USAGE}");
                return ExitCode::SUCCESS;
            }
            _ => {
                // Never echo the argument: it might be a misplaced secret.
                eprintln!("lightvm-mcp: unknown argument\n{USAGE}");
                return ExitCode::from(2);
            }
        }
    }
    let pairing_file = pairing::resolve_path();
    if pairing_file.is_none() {
        eprintln!("lightvm-mcp: no pairing file location; session tools will refuse");
    }
    let rt = match tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("lightvm-mcp: runtime not started: {e}");
            return ExitCode::from(1);
        }
    };
    rt.block_on(async move {
        eprintln!("lightvm-mcp: starting on stdio (MCP 2025-11-25)");
        let adapter = Adapter::new(AdapterConfig {
            pairing_file,
            ..AdapterConfig::default()
        });
        let running = match adapter.serve(stdio()).await {
            Ok(r) => r,
            Err(e) => {
                eprintln!("lightvm-mcp: initialization failed: {e}");
                return ExitCode::from(2);
            }
        };
        match running.waiting().await {
            Ok(_) => {
                eprintln!("lightvm-mcp: client closed the connection");
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("lightvm-mcp: service error: {e}");
                ExitCode::from(1)
            }
        }
    })
}

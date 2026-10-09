//! Binario dello spike MCP: server stdio con tool finti (F1-19, ADR-0021).
//!
//! stdout trasporta soltanto messaggi MCP (JSON-RPC delimitato da newline);
//! la diagnostica va su stderr ed è priva di segreti (ROADMAP sezione 9).

use lightvm_mcp::SpikeServer;
use rmcp::{ServiceExt, transport::stdio};

#[tokio::main(flavor = "current_thread")]
async fn main() -> std::process::ExitCode {
    eprintln!("lightvm-mcp-spike: avvio su stdio (tool finti, nessuna VM reale)");
    let running = match SpikeServer::new().serve(stdio()).await {
        Ok(running) => running,
        Err(err) => {
            eprintln!("lightvm-mcp-spike: inizializzazione fallita: {err}");
            return std::process::ExitCode::from(2);
        }
    };
    match running.waiting().await {
        Ok(reason) => {
            eprintln!("lightvm-mcp-spike: chiusura ({reason:?})");
            std::process::ExitCode::SUCCESS
        }
        Err(err) => {
            eprintln!("lightvm-mcp-spike: errore del servizio: {err}");
            std::process::ExitCode::from(1)
        }
    }
}

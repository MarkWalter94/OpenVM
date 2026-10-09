//! Test di integrazione dello spike MCP (F1-19, ADR-0021).
//!
//! Avvia il binario `lightvm-mcp-spike` come processo figlio e parla JSON-RPC
//! 2.0 delimitato da newline su stdin/stdout, come farebbe un client MCP con
//! trasporto stdio (specifica 2025-11-25, "basic/transports"). Il client è
//! scritto a mano con la sola libreria standard e `serde_json`, così il test
//! non dipende dal codice client dell'SDK che sta provando.
//!
//! I tool sono finti (catalogo statico): il test verifica protocollo,
//! negoziazione, schemi e forma dei risultati, non VM reali.

use std::{
    io::{BufRead, BufReader, Write},
    process::{Child, ChildStdin, Command, Stdio},
    sync::mpsc::{Receiver, RecvTimeoutError, channel},
    thread,
    time::Duration,
};

use serde_json::{Value, json};

const TIMEOUT: Duration = Duration::from_secs(20);

/// Client JSON-RPC minimo sopra stdin/stdout del processo figlio.
struct Client {
    child: Child,
    stdin: Option<ChildStdin>,
    lines: Receiver<String>,
}

impl Client {
    fn spawn() -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_lightvm-mcp-spike"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("avvio del server di prova");
        let stdin = child.stdin.take();
        let stdout = child.stdout.take().expect("stdout del figlio");
        let (tx, rx) = channel();
        thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        // stderr scaricato per non bloccare il figlio; contenuto non verificato.
        let stderr = child.stderr.take().expect("stderr del figlio");
        thread::spawn(move || for _ in BufReader::new(stderr).lines() {});
        Self {
            child,
            stdin,
            lines: rx,
        }
    }

    fn send(&mut self, msg: &Value) {
        let stdin = self.stdin.as_mut().expect("stdin aperto");
        let mut text = serde_json::to_string(msg).expect("serializzazione");
        text.push('\n');
        stdin
            .write_all(text.as_bytes())
            .expect("scrittura su stdin");
        stdin.flush().expect("flush di stdin");
    }

    /// Legge il prossimo messaggio: ogni riga di stdout deve essere JSON-RPC.
    fn recv(&self) -> Value {
        let line = match self.lines.recv_timeout(TIMEOUT) {
            Ok(line) => line,
            Err(RecvTimeoutError::Timeout) => panic!("nessuna risposta entro {TIMEOUT:?}"),
            Err(RecvTimeoutError::Disconnected) => panic!("stdout chiuso dal server"),
        };
        let value: Value = serde_json::from_str(&line)
            .unwrap_or_else(|e| panic!("stdout non contiene solo JSON-RPC: {e}: {line:?}"));
        assert_eq!(
            value["jsonrpc"], "2.0",
            "messaggio non JSON-RPC 2.0: {value}"
        );
        value
    }

    /// Invia una richiesta e attende la risposta con lo stesso `id`.
    fn request(&mut self, id: u64, method: &str, params: Value) -> Value {
        self.send(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
        loop {
            let msg = self.recv();
            if msg.get("id") == Some(&json!(id)) {
                return msg;
            }
            // Notifiche del server (nessuna attesa in questo spike): ignorate.
            assert!(msg.get("method").is_some(), "risposta inattesa: {msg}");
        }
    }

    fn initialize(&mut self, version: &str) -> Value {
        self.request(
            1,
            "initialize",
            json!({
                "protocolVersion": version,
                "capabilities": {},
                "clientInfo": {"name": "lightvm-mcp-test", "version": "0.0.0"}
            }),
        )
    }

    /// Chiude stdin (fine sessione stdio) e attende l'uscita del figlio.
    fn shutdown(mut self) -> std::process::ExitStatus {
        drop(self.stdin.take());
        for _ in 0..200 {
            if let Some(status) = self.child.try_wait().expect("stato del figlio") {
                return status;
            }
            thread::sleep(Duration::from_millis(50));
        }
        let _ = self.child.kill();
        panic!("il server non termina dopo la chiusura di stdin");
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn initialize_tools_list_e_tools_call() {
    let mut client = Client::spawn();

    // 1. initialize con la baseline 2025-11-25.
    let init = client.initialize("2025-11-25");
    let result = &init["result"];
    assert_eq!(result["protocolVersion"], "2025-11-25", "{init}");
    assert_eq!(result["serverInfo"]["name"], "lightvm-mcp-spike");
    assert!(result["capabilities"]["tools"].is_object(), "{init}");
    client.send(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));

    // 2. tools/list: due tool con JSON Schema di ingresso e di uscita.
    let list = client.request(2, "tools/list", json!({}));
    let tools = list["result"]["tools"].as_array().expect("elenco dei tool");
    let mut names: Vec<&str> = tools.iter().filter_map(|t| t["name"].as_str()).collect();
    names.sort_unstable();
    assert_eq!(names, ["vm_list", "vm_status"], "{list}");
    for tool in tools {
        assert_eq!(tool["inputSchema"]["type"], "object", "{tool}");
        assert_eq!(tool["outputSchema"]["type"], "object", "{tool}");
        assert!(tool["description"].as_str().is_some_and(|d| !d.is_empty()));
    }
    let status = tools
        .iter()
        .find(|t| t["name"] == "vm_status")
        .expect("vm_status");
    assert_eq!(
        status["inputSchema"]["required"],
        json!(["vm_id"]),
        "{status}"
    );
    assert_eq!(
        status["inputSchema"]["properties"]["vm_id"]["type"], "string",
        "{status}"
    );
    // Dialetto predefinito della specifica 2025-11-25 e nessun campo extra.
    assert_eq!(
        status["inputSchema"]["$schema"], "https://json-schema.org/draft/2020-12/schema",
        "{status}"
    );
    assert_eq!(status["inputSchema"]["additionalProperties"], false);

    // 3. tools/call vm_list: risultato strutturato.
    let call = client.request(3, "tools/call", json!({"name": "vm_list", "arguments": {}}));
    let r = &call["result"];
    assert_ne!(r["isError"], true, "{call}");
    let vms = r["structuredContent"]["vms"].as_array().expect("vms");
    assert_eq!(vms.len(), 2, "{call}");
    assert_eq!(vms[0]["vm_id"], "vm-demo-1");
    // Compatibilità all'indietro della specifica: anche contenuto testuale.
    assert_eq!(r["content"][0]["type"], "text", "{call}");

    // 4. tools/call vm_status su una VM esistente.
    let call = client.request(
        4,
        "tools/call",
        json!({"name": "vm_status", "arguments": {"vm_id": "vm-demo-2"}}),
    );
    let r = &call["result"];
    assert_ne!(r["isError"], true, "{call}");
    assert_eq!(r["structuredContent"]["state"], "running", "{call}");
    assert_eq!(r["structuredContent"]["simulated"], true, "{call}");

    // 5. Errore operativo: VM sconosciuta → isError nel risultato, non JSON-RPC.
    let call = client.request(
        5,
        "tools/call",
        json!({"name": "vm_status", "arguments": {"vm_id": "inesistente"}}),
    );
    assert!(call.get("error").is_none(), "{call}");
    assert_eq!(call["result"]["isError"], true, "{call}");

    // 6. Errore di protocollo: tool sconosciuto → errore JSON-RPC.
    let call = client.request(
        6,
        "tools/call",
        json!({"name": "vm_delete", "arguments": {}}),
    );
    assert!(call.get("result").is_none(), "{call}");
    assert!(call["error"]["code"].is_i64(), "{call}");

    // 7. Argomenti non validi (campo sconosciuto): errore di esecuzione del
    // tool (`isError`), come chiede la specifica 2025-11-25 per la validazione
    // degli input; nessun contenuto strutturato.
    let call = client.request(
        7,
        "tools/call",
        json!({"name": "vm_status", "arguments": {"vm_id": "vm-demo-1", "extra": 1}}),
    );
    assert_eq!(call["result"]["isError"], true, "{call}");
    assert!(call["result"].get("structuredContent").is_none(), "{call}");

    let status = client.shutdown();
    assert!(status.success(), "uscita del server: {status:?}");
}

/// Un client che chiede una revisione diversa riceve la sola revisione
/// supportata (regola di negoziazione della specifica 2025-11-25, "basic/
/// lifecycle"): il server non accetta mai un'altra versione.
#[test]
fn versione_precedente_riceve_la_baseline() {
    let mut client = Client::spawn();
    let init = client.initialize("2025-06-18");
    assert_eq!(init["result"]["protocolVersion"], "2025-11-25", "{init}");
    client.shutdown();
}

/// Una revisione successiva senza handshake (`_meta` per richiesta,
/// 2026-07-28) è rifiutata con l'errore "versione non supportata" dell'SDK.
#[test]
fn revisione_successiva_senza_initialize_rifiutata() {
    let mut client = Client::spawn();
    let reply = client.request(
        1,
        "tools/list",
        json!({
            "_meta": {
                "io.modelcontextprotocol/protocolVersion": "2026-07-28",
                "io.modelcontextprotocol/clientCapabilities": {}
            }
        }),
    );
    assert!(reply.get("result").is_none(), "{reply}");
    assert_eq!(reply["error"]["code"], -32022, "{reply}");
    assert_eq!(
        reply["error"]["data"]["supported"],
        json!(["2025-11-25"]),
        "{reply}"
    );
    client.shutdown();
}

/// Una richiesta diversa da `initialize` senza metadati, prima
/// dell'handshake, non viene eseguita.
#[test]
fn richiesta_prima_di_initialize_non_eseguita() {
    let mut client = Client::spawn();
    let reply = client.request(1, "tools/call", json!({"name": "vm_list", "arguments": {}}));
    assert!(reply.get("result").is_none(), "{reply}");
    assert!(reply["error"]["code"].is_i64(), "{reply}");
    client.shutdown();
}

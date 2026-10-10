//! Integration test of the MCP spike (F1-19, ADR-0021).
//!
//! Starts the `lightvm-mcp-spike` binary as a child process and speaks
//! newline-delimited JSON-RPC 2.0 over stdin/stdout, as an MCP client with
//! stdio transport would (specification 2025-11-25, "basic/transports"). The
//! client is hand-written with only the standard library and `serde_json`, so
//! the test does not depend on the client code of the SDK it is testing.
//!
//! The tools are fake (static catalog): the test verifies protocol,
//! negotiation, schemas and result shape, not real VMs.

use std::{
    io::{BufRead, BufReader, Write},
    process::{Child, ChildStdin, Command, Stdio},
    sync::mpsc::{Receiver, RecvTimeoutError, channel},
    thread,
    time::Duration,
};

use serde_json::{Value, json};

const TIMEOUT: Duration = Duration::from_secs(20);

/// Minimal JSON-RPC client over the child process's stdin/stdout.
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
            .expect("test server start");
        let stdin = child.stdin.take();
        let stdout = child.stdout.take().expect("child stdout");
        let (tx, rx) = channel();
        thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        // stderr drained so as not to block the child; content not checked.
        let stderr = child.stderr.take().expect("child stderr");
        thread::spawn(move || for _ in BufReader::new(stderr).lines() {});
        Self {
            child,
            stdin,
            lines: rx,
        }
    }

    fn send(&mut self, msg: &Value) {
        let stdin = self.stdin.as_mut().expect("stdin open");
        let mut text = serde_json::to_string(msg).expect("serialization");
        text.push('\n');
        stdin.write_all(text.as_bytes()).expect("write to stdin");
        stdin.flush().expect("stdin flush");
    }

    /// Reads the next message: every stdout line must be JSON-RPC.
    fn recv(&self) -> Value {
        let line = match self.lines.recv_timeout(TIMEOUT) {
            Ok(line) => line,
            Err(RecvTimeoutError::Timeout) => panic!("no response within {TIMEOUT:?}"),
            Err(RecvTimeoutError::Disconnected) => panic!("stdout closed by the server"),
        };
        let value: Value = serde_json::from_str(&line)
            .unwrap_or_else(|e| panic!("stdout does not contain only JSON-RPC: {e}: {line:?}"));
        assert_eq!(
            value["jsonrpc"], "2.0",
            "message is not JSON-RPC 2.0: {value}"
        );
        value
    }

    /// Sends a request and waits for the response with the same `id`.
    fn request(&mut self, id: u64, method: &str, params: Value) -> Value {
        self.send(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
        loop {
            let msg = self.recv();
            if msg.get("id") == Some(&json!(id)) {
                return msg;
            }
            // Server notifications (none expected in this spike): ignored.
            assert!(msg.get("method").is_some(), "unexpected response: {msg}");
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

    /// Closes stdin (end of the stdio session) and waits for the child to exit.
    fn shutdown(mut self) -> std::process::ExitStatus {
        drop(self.stdin.take());
        for _ in 0..200 {
            if let Some(status) = self.child.try_wait().expect("child status") {
                return status;
            }
            thread::sleep(Duration::from_millis(50));
        }
        let _ = self.child.kill();
        panic!("the server does not terminate after stdin is closed");
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn initialize_tools_list_and_tools_call() {
    let mut client = Client::spawn();

    // 1. initialize with the 2025-11-25 baseline.
    let init = client.initialize("2025-11-25");
    let result = &init["result"];
    assert_eq!(result["protocolVersion"], "2025-11-25", "{init}");
    assert_eq!(result["serverInfo"]["name"], "lightvm-mcp-spike");
    assert!(result["capabilities"]["tools"].is_object(), "{init}");
    client.send(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));

    // 2. tools/list: two tools with input and output JSON Schema.
    let list = client.request(2, "tools/list", json!({}));
    let tools = list["result"]["tools"].as_array().expect("tool list");
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
    // Default dialect of the 2025-11-25 specification and no extra fields.
    assert_eq!(
        status["inputSchema"]["$schema"], "https://json-schema.org/draft/2020-12/schema",
        "{status}"
    );
    assert_eq!(status["inputSchema"]["additionalProperties"], false);

    // 3. tools/call vm_list: structured result.
    let call = client.request(3, "tools/call", json!({"name": "vm_list", "arguments": {}}));
    let r = &call["result"];
    assert_ne!(r["isError"], true, "{call}");
    let vms = r["structuredContent"]["vms"].as_array().expect("vms");
    assert_eq!(vms.len(), 2, "{call}");
    assert_eq!(vms[0]["vm_id"], "vm-demo-1");
    // Backward compatibility of the specification: text content too.
    assert_eq!(r["content"][0]["type"], "text", "{call}");

    // 4. tools/call vm_status on an existing VM.
    let call = client.request(
        4,
        "tools/call",
        json!({"name": "vm_status", "arguments": {"vm_id": "vm-demo-2"}}),
    );
    let r = &call["result"];
    assert_ne!(r["isError"], true, "{call}");
    assert_eq!(r["structuredContent"]["state"], "running", "{call}");
    assert_eq!(r["structuredContent"]["simulated"], true, "{call}");

    // 5. Operational error: unknown VM → isError in the result, not JSON-RPC.
    let call = client.request(
        5,
        "tools/call",
        json!({"name": "vm_status", "arguments": {"vm_id": "nonexistent"}}),
    );
    assert!(call.get("error").is_none(), "{call}");
    assert_eq!(call["result"]["isError"], true, "{call}");

    // 6. Protocol error: unknown tool → JSON-RPC error.
    let call = client.request(
        6,
        "tools/call",
        json!({"name": "vm_delete", "arguments": {}}),
    );
    assert!(call.get("result").is_none(), "{call}");
    assert!(call["error"]["code"].is_i64(), "{call}");

    // 7. Invalid arguments (unknown field): tool execution error
    // (`isError`), as the 2025-11-25 specification requires for input
    // validation; no structured content.
    let call = client.request(
        7,
        "tools/call",
        json!({"name": "vm_status", "arguments": {"vm_id": "vm-demo-1", "extra": 1}}),
    );
    assert_eq!(call["result"]["isError"], true, "{call}");
    assert!(call["result"].get("structuredContent").is_none(), "{call}");

    let status = client.shutdown();
    assert!(status.success(), "server exit: {status:?}");
}

/// A client that asks for a different revision receives the only supported
/// revision (negotiation rule of the 2025-11-25 specification, "basic/
/// lifecycle"): the server never accepts another version.
#[test]
fn previous_version_receives_the_baseline() {
    let mut client = Client::spawn();
    let init = client.initialize("2025-06-18");
    assert_eq!(init["result"]["protocolVersion"], "2025-11-25", "{init}");
    client.shutdown();
}

/// A later revision without handshake (per-request `_meta`, 2026-07-28) is
/// rejected with the SDK's "unsupported version" error.
#[test]
fn later_revision_without_initialize_rejected() {
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

/// A request other than `initialize` without metadata, before the
/// handshake, is not executed.
#[test]
fn request_before_initialize_not_executed() {
    let mut client = Client::spawn();
    let reply = client.request(1, "tools/call", json!({"name": "vm_list", "arguments": {}}));
    assert!(reply.get("result").is_none(), "{reply}");
    assert!(reply["error"]["code"].is_i64(), "{reply}");
    client.shutdown();
}

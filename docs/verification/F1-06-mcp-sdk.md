# F1-06 — Rust MCP SDK: selection and test over stdio with revision 2025-11-25

- Execution date (UTC): 2026-10-09
- Executed by: development agent (Claude Code session), at the request of the project owner
- Roadmap step: section 9 (MCP baseline 2025-11-25, official Rust SDK, rejection of unsupported negotiations, stdout MCP-only); section 10 F0 ("Closable within F1: MCP SDK/clients compatible with 2025-11-25") and F1; backlog F1-19 in `docs/RELEASE_PLAN.md`
- Related ADRs: ADR-0021 (updated), ADR-0023 (licences)
- Overall outcome: **partial** — SDK chosen and tested over stdio with a hand-written JSON-RPC client, on Windows and on Linux (WSL2); **no real MCP client tested** (required by ADR-0021 and by F5)

Allowed outcomes: **verified**, **failed**, **not executed** (section 17). Raw logs in `lab/logs/f1-06/` (outside the repository): `test-windows.txt`, `test-linux-wsl.txt`, `trascrizione.txt`, `negoziazione.txt`, `tree-licenze.txt`, `comandi-finali.txt`, `check-workspace-errore.txt`.

## Purpose

Choose the release of the Rust SDK for MCP to use in the stdio adapter (F5) and test, before writing product code, that it:

1. supports revision 2025-11-25 with the stdio transport;
2. makes it possible to restrict negotiation to 2025-11-25 only;
3. publishes tools with input and output JSON Schemas and returns structured results;
4. distinguishes operational errors (result with `isError: true`) from protocol errors (JSON-RPC error), as section 9 requires;
5. writes only MCP messages to stdout;
6. has dependencies with licences compatible with MIT distribution (ADR-0023) and no network dependency in the server.

It does not demonstrate: compatibility with real MCP clients (none tested), image content (`image`) in results, connection to the supervisor, authorization, grants and leases (F4–F5), performance, robustness against adversarial input beyond the cases listed below.

## Environment

| Item | Value |
| --- | --- |
| Host | F0 host of ADR-0018 (Intel Core i7-12800HX), shared with other agents |
| OS and build | Windows 11 Pro 10.0.26200; Linux in WSL2, kernel 6.18.33.1-microsoft-standard-WSL2, Ubuntu (default distro) |
| Accelerator and state | not relevant |
| Toolchain / relevant versions | Rust 1.99.0 (`x86_64-pc-windows-msvc`, Build Tools 2022 environment loaded with `vcvars64.bat`); in WSL `cargo 1.99.0` with the zig linker of `lab/wsl/env.sh` (F0-07); Node v24.15.0 for the transcripts |
| SDK | `rmcp` 3.5.1 from crates.io, features `server`, `macros`, `transport-io` (without the defaults) |
| QEMU binary, firmware, guest | not relevant |
| Graphics session | not relevant (console process) |
| Network | only crates.io through cargo (`cargo info`, crate downloads); no access to GitHub or other sites for this test |

## Comparison of candidates

Source: `cargo info <crate>` on crates.io, 2026-10-09. The roadmap (section 9) indicates the official Rust SDK; the alternatives are needed only if that one does not support the required revision (ADR-0021).

| Crate | Version | Licence | Origin | Assessment |
| --- | --- | --- | --- | --- |
| `rmcp` | 3.5.1 | Apache-2.0 | `modelcontextprotocol/rust-sdk` (official SDK), `rust-version` 1.88 | **chosen**: published release; constants for 2024-11-05, 2025-03-26, 2025-06-18, **2025-11-25**, 2026-07-28; list of supported versions can be restricted by the server; stdio transport with a separate feature; network features (HTTP, OAuth, child process) all optional |
| `mcp-server` | 0.1.0 | MIT | same `repository` field as `rmcp` | excluded without testing: the current releases of the official SDK are published as `rmcp`; not investigated further |
| `rust-mcp-sdk` | 2.0.0 | MIT | `rust-mcp-stack` (unofficial) | not tested: unofficial; support for 2025-11-25 not verified |
| `pmcp` | 2.22.7 | MIT | `paiml/pmcp` (unofficial) | not tested: unofficial; support for 2025-11-25 not verified |
| `mcp-core` | 0.1.50 | Apache-2.0 | unofficial | not tested |
| `ultrafast-mcp` | 202506018.1.0 | MIT OR Apache-2.0 | unofficial | not tested; non-semantic versioning scheme |
| In-house implementation | — | MIT | — | not needed: the official SDK supports the revision |

Important observation: in `rmcp` 3.5.1 `ProtocolVersion::LATEST` is **2026-07-28** (revision without `initialize`, with per-request metadata) and the default list of supported versions includes them all, from 2024-11-05 to 2026-07-28. Without an explicit restriction the server would accept revisions different from the baseline: the test server overrides `supported_protocol_versions` with 2025-11-25 only.

## Procedure

1. Crate `crates/lightvm-mcp` (workspace member through the `crates/*` glob, no change to the root `Cargo.toml`), without `unsafe`, with the workspace lints:
   - `src/lib.rs`: `SpikeServer` with `#[tool_router]`/`#[tool_handler]`; tools `vm_list` (no arguments, result `{ vms: [...] }`) and `vm_status` (`{ vm_id }` with `deny_unknown_fields`, length 1–64; structured result with `simulated: true`); static fake catalog; unknown VM → `Err(String)` (result with `isError: true`); `get_info` with `protocol_version = 2025-11-25` and capability `tools`; `supported_protocol_versions` = `[2025-11-25]`.
   - `src/main.rs`: binary `lightvm-mcp-spike`, `serve(stdio())` on a single-threaded tokio runtime; diagnostics only on stderr.
   - Dependencies with exact versions: `rmcp =3.5.1` (`default-features = false`, `server`, `macros`, `transport-io`), `schemars =1.2.2`, `serde =1.0.229`, `serde_json =1.0.151`, `tokio =1.53.2` (`io-std`, `macros`, `rt`), aligned with those already present in the lockfile.
2. Integration test `tests/stdio.rs`: starts the binary as a child process (`CARGO_BIN_EXE_lightvm-mcp-spike`) and speaks newline-delimited JSON-RPC 2.0 with a client written with only the standard library and `serde_json` (it does not use the SDK's client code). 20 s timeout per response. Every stdout line must be JSON-RPC 2.0. Tests:
   - `initialize_tools_list_e_tools_call`: `initialize` (2025-11-25) → `notifications/initialized` → `tools/list` → `tools/call` of `vm_list`, existing `vm_status`, unknown `vm_status`, nonexistent tool, extra argument; then closing stdin and exit with code 0;
   - `versione_precedente_riceve_la_baseline`: `initialize` with 2025-06-18;
   - `revisione_successiva_senza_initialize_rifiutata`: `tools/list` with the `_meta` of 2026-07-28;
   - `richiesta_prima_di_initialize_non_eseguita`: `tools/call` without handshake.
3. Commands (Windows, from the root, with the MSVC environment loaded):

   ```text
   cargo fmt -p lightvm-mcp -- --check
   cargo clippy -p lightvm-mcp --all-targets -- -D warnings
   cargo test -p lightvm-mcp            # 3 runs
   cargo check --workspace --all-targets
   ```

4. Linux (WSL2): `. lab/wsl/env.sh && cargo test -p lightvm-mcp --locked`, 3 runs.
5. Protocol transcripts with a Node script that sends the messages on the stdin of the debug binary and records stdout and stderr (`trascrizione.txt`, `negoziazione.txt`).
6. Dependency licences: `cargo tree -p lightvm-mcp -e normal --prefix none --format "{p} {l}"` (host target) and `cargo deny check` on the workspace (see `F1-07-licences-packaging.md`).

## Results

### 1. Tests

| Item | Outcome | Evidence |
| --- | --- | --- |
| Unit tests (`solo_la_baseline_e_supportata`, `catalogo_dichiarato_simulato`) | verified | Windows and WSL2: `2 passed` in 3 runs out of 3 |
| Integration tests over stdio (4 tests) | verified | Windows: `4 passed` in 3 runs out of 3 (`test-windows.txt`); WSL2: `4 passed` in 3 runs out of 3 (`test-linux-wsl.txt`) |
| `cargo fmt -p lightvm-mcp -- --check` | verified | `EXIT=0` |
| `cargo clippy -p lightvm-mcp --all-targets -- -D warnings` | verified | no warnings, `EXIT=0` |
| `cargo check --workspace --all-targets` (first run) | **failed** for causes external to this crate | `crates\lightvm-service\tests\ipc.rs:10:22` `unresolved import lightvm_service::compat` and `:19:34` `unresolved import lightvm_service::supervisor::fake` (work in progress by another agent; `check-workspace-errore.txt`) |
| `cargo check --workspace --all-targets` (repetition, 15:45 UTC) | verified | `Finished`, `EXIT=0` (`comandi-finali.txt`; section 5) |

### 2. Protocol (transcript on Windows)

| Step | Outcome | Observed response |
| --- | --- | --- |
| `initialize` 2025-11-25 | verified | `protocolVersion: "2025-11-25"`, `capabilities: {"tools":{}}`, `serverInfo.name: "lightvm-mcp-spike"` |
| `tools/list` | verified | `vm_list` and `vm_status` with `inputSchema` and `outputSchema` of type `object`; dialect `https://json-schema.org/draft/2020-12/schema` (default in 2025-11-25); `vm_status` with `required: ["vm_id"]`, `additionalProperties: false`, `minLength: 1`, `maxLength: 64` |
| `tools/call vm_list` | verified | `structuredContent.vms` with 2 entries, textual `content[0]` with the same JSON, `isError: false` |
| `tools/call vm_status` (`vm-demo-1`) | verified | `structuredContent` with `state: "stopped"`, `simulated: true` |
| Unknown VM (operational error) | verified | `result.isError: true`, text `vm_not_found: …`, no JSON-RPC error |
| Nonexistent tool (protocol error) | verified | `error.code: -32602`, `message: "tool not found"` |
| Unknown argument (`extra`) | verified | `result.isError: true`, `failed to deserialize parameters: unknown field 'extra', expected 'vm_id'`; no `structuredContent` (tool execution error, as 2025-11-25 requires for input validation) |
| stdout MCP-only | verified | every stdout line is JSON-RPC 2.0 (test assertion); stderr contains only `avvio su stdio…` and `chiusura (Closed)` |
| Closing stdin | verified | the server terminates with code 0 after stdin is closed |

### 3. Version negotiation (`negoziazione.txt`)

| Client request | Outcome | Response |
| --- | --- | --- |
| `initialize` with 2025-06-18 | verified | result with `protocolVersion: "2025-11-25"` |
| `initialize` with 2026-07-28 | verified | result with `protocolVersion: "2025-11-25"` |
| `initialize` with `1999-01-01` | verified | result with `protocolVersion: "2025-11-25"` |
| `tools/list` with `_meta` 2026-07-28, without `initialize` | verified | `error.code: -32022`, `Unsupported protocol version`, `data.supported: ["2025-11-25"]` |
| `server/discover` with `_meta` 2026-07-28 | verified | same error `-32022` |
| `tools/call` before `initialize`, without `_meta` | verified | `error.code: -32602` (mandatory metadata missing); the tool is not executed |
| JSON value that is not an object (`"{non json"`) | verified | `error.code: -32600`, `Invalid request` |

The server never concludes a session with a version other than 2025-11-25. With `initialize` the rule of the 2025-11-25 specification ("basic/lifecycle") requires answering with another supported version, not with an error: the actual rejection is up to the client, which must disconnect if it does not support the proposed version. Requests in the 2026-07-28 format instead receive an explicit error.

### 4. Licences of the SDK's dependencies

`cargo tree -p lightvm-mcp -e normal` (Windows host target): 55 distinct third-party packages, proc-macros included (`tree-licenze.txt`).

| Declared licence | Packages |
| --- | --- |
| `MIT OR Apache-2.0` | 31 |
| `MIT` | 15 (including `tokio`, `tokio-util`, `tracing*`, `schemars*`, `darling*`, `bytes`, `slab`, `strsim`, `zmij`) |
| `Apache-2.0 OR MIT` | 4 |
| `Apache-2.0` | 2 (`rmcp`, `rmcp-macros`) |
| `Unlicense OR MIT` | 1 (`memchr`) |
| `MIT/Apache-2.0` | 1 (`ident_case`) |
| `(MIT OR Apache-2.0) AND Unicode-3.0` | 1 (`unicode-ident`, only for procedural macros) |

No copyleft licence. `rmcp` is Apache-2.0 only: compatible with distributing the binary under MIT, provided its text and notices are reproduced in the third-party inventory (F6). No network dependency in the graph: no `reqwest`, `hyper`, `oauth2`, TLS; `deny.toml` now bans these `rmcp` features and these crates (F1-07). `uuid` and `getrandom` come from the SDK's `server` feature.

### 5. Final repetition of the commands (2026-10-09 15:45 UTC, `comandi-finali.txt`)

`cargo fmt -p lightvm-mcp -- --check` `EXIT=0`; `cargo clippy -p lightvm-mcp --all-targets -- -D warnings` without warnings; `cargo test -p lightvm-mcp` `2 passed` + `4 passed`; `cargo check --workspace --all-targets` `EXIT=0`.

## Limits

- **No real MCP client.** The test client is hand-written: it demonstrates the server's conformance to the messages used, not interoperability with existing clients. ADR-0021 and section 9 require two real clients with a documented version (F5).
- Image content (`type: "image"`), `tools/list_changed` notifications, cancellation, progress, `ping` and message size limits not tested.
- The SDK does not validate arguments against the published schema: `minLength`/`maxLength` are only declared; actual validation remains the adapter's responsibility (section 9: "structured results validated against the schemas").
- The SDK's error messages are in English and include serde's deserialization details; for public messages the adapter will have to map the errors onto the stable codes of `lightvm-proto`.
- Reading stdin: the SDK's stdio transport was not tested with very large messages or with hostile input (fuzzing planned by section 13).
- The SDK is young and changes quickly (major 3 with revision 2026-07-28): every update must be retested with this test; Dependabot proposes it as a `cargo` update subject to review.
- In WSL2 the build uses the zig linker (F0-07), not Ubuntu's standard toolchain.
- Host shared with other agents; the workspace was evolving during the test.

## Required actions

1. F5: test at least two real MCP clients with the stdio transport (version, OS, date) and update `docs/COMPATIBILITY.md`, section "MCP clients tested".
2. F5: in the adapter, validate the arguments against the schemas generated by `lightvm-proto` before forwarding them to the supervisor and map errors onto stable public codes.
3. Test image content with byte limits (section 9) and the behaviour of the chosen clients.
4. Targeted fuzzing of the stdio transport and of parameter deserialization (section 13).
5. At every `rmcp` update: repeat `cargo test -p lightvm-mcp` and verify that `supported_protocol_versions` stays limited to the baseline.

## Proposed decisions

- **ADR-0021:** adopt `rmcp` 3.5.1 (`=3.5.1`, features `server`, `macros`, `transport-io`, without defaults), with `supported_protocol_versions` limited to `2025-11-25`; the HTTP/OAuth/client features remain banned in `deny.toml`. Choice of clients deferred to F5.
- Behaviour on unsupported versions: with `initialize`, counter-proposal of 2025-11-25 as the specification requires (the client decides whether to disconnect); requests in the per-request format of later revisions rejected with `-32022`. If an explicit error is wanted on `initialize` too, a justified deviation from the specification is needed.

## Files produced

- `crates/lightvm-mcp/Cargo.toml`, `src/lib.rs`, `src/main.rs`, `tests/stdio.rs`
- `Cargo.lock` (updated by cargo with the new dependencies)
- `docs/verification/F1-06-mcp-sdk.md` (this file)
- `lab/logs/f1-06/*` (outside the repository)

# ADR-0021 — MCP SDK and clients

- Status: Accepted for the SDK (`rmcp` 3.5.1, evidence in `docs/verification/F1-06-mcp-sdk.md`); choice of MCP clients open until tested with real clients (F5)
- Date: 2026-10-09
- Owner: to be assigned
- To be closed by: F1 for the SDK (closed); F5 for the two reference clients

## Context

MCP protocol baseline 2025-11-25. In F0 a release of the official Rust SDK that supports it is selected, tested with the versions of the chosen clients; unsupported negotiations are rejected; no dependency on the main branch (roadmap, section 9). F5 requires at least two real clients with documented versions.

## Decision

**SDK:** `rmcp` **3.5.1** from crates.io (official Rust SDK, repository `modelcontextprotocol/rust-sdk`, Apache-2.0 license), declared with the exact version `=3.5.1`, `default-features = false` and only the `server`, `macros`, `transport-io` features. No git dependency nor dependency on the main branch; workspace lockfile.

Usage rules:

1. The server restricts `supported_protocol_versions` to **2025-11-25** only: in `rmcp` 3.5.1 the default list includes all known revisions (from 2024-11-05 to 2026-07-28, which is `ProtocolVersion::LATEST`), so without the restriction the server would accept revisions other than the baseline.
2. Unsupported versions: on `initialize` the server responds with 2025-11-25, as the 2025-11-25 specification requires (a client that does not support it must disconnect); requests in the per-request format of later revisions (`_meta` metadata, 2026-07-28) are rejected with error `-32022` "Unsupported protocol version". No session is established with a version other than the baseline. An explicit error on `initialize` as well would require a deviation from the specification, to be justified in an update of this ADR.
3. Operational errors as a tool result with `isError: true`; protocol errors as a JSON-RPC error (unknown tool → `-32602`); invalid arguments as a tool execution error (2025-11-25 rule).
4. stdout contains only MCP; diagnostics on stderr.
5. The SDK's network features (HTTP, SSE, OAuth, client) are banned in `deny.toml` (ADR-0023).
6. The SDK does not validate arguments against the published schema: validation stays with the adapter, using the `lightvm-proto` schemas.
7. Every `rmcp` upgrade repeats the integration test of the test crate (`crates/lightvm-mcp`) before being accepted.

**Clients:** to be decided in F5. To be recorded: two reference MCP clients with version, OS and date of verification; client behaviour on the version counter-proposal. No client is considered compatible before testing.

Criteria:

1. Published release, not main branch; lockfile. — met (3.5.1 from crates.io).
2. stdio transport working with both clients on the target host. — server on stdio verified with a test JSON-RPC client on Windows and on Linux (WSL2); real clients not tested.
3. Image content and structured results handled by the clients as the specification requires. — structured results verified server side; image content and clients not tested.
4. Compatible license (ADR-0023). — met (Apache-2.0; all dependencies permissive, `F1-06` §4).

## Alternatives considered

- Official Rust SDK (indicated by the roadmap) versus an in-house protocol implementation: the latter only if the SDK does not support the required revision; not needed, because `rmcp` 3.5.1 supports 2025-11-25.
- Unofficial SDKs on crates.io (`rust-mcp-sdk` 2.0.0, `pmcp` 2.22.7, `mcp-core` 0.1.50, `ultrafast-mcp`): not tested; rejected because the roadmap indicates the official SDK and it meets the criteria.
- Clients: candidates with stdio support to be identified and tested in F5.

## Consequences

- The `mcp-server` crate is added in F5 (section 6.2); `crates/lightvm-mcp` is only the SDK test bench (dummy tools `vm_list`, `vm_status`) and is not the product adapter.
- The SDK changes quickly (major 3 introduced with the 2026-07-28 revision): upgrades proposed by Dependabot remain subject to review and to the integration test.
- Example configurations published only for clients actually tested (section 9).

## Required evidence

- SDK: `docs/verification/F1-06-mcp-sdk.md` (verified on stdio with a test client; Windows and Linux in WSL2).
- Clients: dedicated verification file in F5 with at least two real clients.

## References

Roadmap, sections 3, 6.2, 9, 10 (F0, F1, F5), 14, 17.

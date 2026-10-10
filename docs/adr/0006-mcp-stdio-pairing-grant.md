# ADR-0006 — MCP over stdio with pairing and a limited grant, no generic QMP

- Status: Accepted
- Date: 2026-10-09
- Owner: to be assigned

## Context

With the stdio transport the MCP client launches the server as a subprocess; LightVM cannot govern that process, but it can govern what the adapter obtains from the supervisor. QMP is a privileged interface capable of accessing the host filesystem (roadmap, sections 3 and 9).

## Decision

- MCP protocol baseline 2025-11-25; `lightvm-mcp` adapter over stdio, no HTTP transport in the MVP.
- The adapter does not open disks or run QEMU: it forwards to the supervisor via a named pipe with ACLs (Windows) or a socket with restrictive permissions (Unix).
- MCP disabled by default at every startup; no authorization survives a restart.
- The first connection only creates a pairing request with an identity and a code that can be verified in the app; the limited grant (VM, tools, duration, quotas) is issued by the supervisor after user approval. Pairing alone grants no permissions.
- Expiry and lease checked by the supervisor; a lease without a valid renewal lasts no more than 10 seconds (section 7.3).
- No `qmp.execute` tool, no host passthrough, no host paths, no media management, no policy changes.
- v0.2 catalog as per section 9; `vm.create`, `vm.delete` and guest tools deferred to R4.
- Credentials in the OS credential store; never in process arguments, reports or examples; never propagated to QEMU.
- `stdout` for MCP only; diagnostics on `stderr` without secrets.

## Alternatives considered

- Streamable HTTP with OAuth: not adopted for the MVP; MCP treats the two transports differently (section 9).
- Generic QMP forwarding: excluded (sections 3 and 9).
- Adapter with direct access to VM files: excluded (section 6.1).

## Consequences

- SDK and reference clients to be selected (ADR-0021).
- Rate limits, bounded queues, timeouts without blind retries, byte limits on screenshots.
- LightVM does not limit what the client already has outside LightVM (section 7.1): this must be declared to the user.

## Required evidence

- "Sessions", "VM", "Host", "Policy", "Lifecycle" negative tests from section 7.4: F4/F5.
- F5 exit criterion with two documented real clients.

## References

Roadmap, sections 3, 4, 6.1, 7.1, 7.3, 7.4, 9, 10 (F4, F5).

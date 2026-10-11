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

### Deviation recorded in F4 (2026-10-11): pairing credential in a user-only file

The pairing credential is delivered to the agent client through `%LOCALAPPDATA%\LightVM\pairing\pairing.json` (`format: "lightvm-pairing-v1"`, endpoint, credential, expiry), not through the OS credential store: the application forbids `unsafe` and the Windows Credential Manager is not reachable without a new FFI dependency. Mitigations: the folder has inheritance removed and exactly one ACE for the user's SID (the app refuses to write otherwise; the MCP adapter checks it before reading); written via temp file and rename; the credential expires after 10 minutes unless used and grants no VM permission on its own (a session still needs the user's approval with the verification code); the file is removed when agent access is turned off, after an Emergency Stop, when the service stops and when a new service starts; never passed as a process argument or logged. A same-user process can read it, as it could read a Credential Manager entry of the same user. Evidence: `docs/verification/F4-02-sessions-ui.md`. Revisit when an audited credential-store binding is accepted (ADR-0023).

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

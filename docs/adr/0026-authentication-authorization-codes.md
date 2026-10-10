# ADR-0026 — Public codes for authentication and authorization

- Status: Accepted (2026-10-10, project owner)
- Date: 2026-10-09; acceptance: 2026-10-10
- Owner: project owner

## Context

Section 6.3 of the roadmap sets stable public errors (`permission_denied`, `session_expired`, `vm_busy`, `unsupported`, `resource_limit`, `state_conflict`) and section 9 asks for operational errors distinct from protocol errors. Section 7.4 asks for a denial without effect for a token that is missing, expired, revoked or reused after a restart, and for the connection to be refused for a process without a credential, without naming a code.

Until this ADR `lightvm-proto` used `permission_denied` both for missing or invalid credentials and for identified but unauthorized callers, and additionally had a **protocol** code `unauthenticated` for the IPC credential. The IPC service (`docs/verification/F1-04-service-ipc-ci.md`) uses `permission_denied` for the handshake and flags the difference as an item to align.

## Decision

1. New operational code `unauthenticated` in `ErrorCode`, appended at the end (the nine previous codes remain unchanged, schema regenerated).
2. Order and meaning of the three codes:
   - `unauthenticated`: no valid credential. IPC credential or session token missing, malformed, unknown, revoked, reused after a supervisor restart, session terminated for any cause other than expiry, request before the handshake. Same code and same message for all reasons.
   - `session_expired`: credential recognized but with expired grant or lease, including after termination due to expiry. Takes precedence over pause.
   - `permission_denied`: caller identified but not authorized. VM outside the allowlist (even if non-existent), capability missing from the effective policy, mutation during pause, insufficient IPC role, VM creation by an agent, medium different from the approved one.
3. The protocol code `ProtocolErrorCode::Unauthenticated` stays in the contract for compatibility but is not emitted: a rejected credential is the operational outcome of an understood request.
4. None of the three codes is automatically retryable (`retryable = false`).

Implementation and tests in `vm-core` for agent sessions; evidence in `docs/verification/F1-01-proto-core.md`.

## Alternatives considered

- **Only `permission_denied`** for all denials: it is the most conservative choice in terms of information, but it prevents the client from distinguishing "a new authorization in the app is needed" from "this action is not in the grant" and conflates denials in the audit.
- **`unauthenticated` as a protocol error**: already present, but section 9 separates protocol errors (request not understood) from operational ones; a rejected credential is not a format problem.
- **Distinguishing missing, wrong and revoked token**: excluded, it would reveal to the caller which sessions exist or have existed.

## Consequences

- `lightvm-service`: the handshake with a missing, malformed, unknown or revoked credential and requests before `hello` should return `unauthenticated` instead of `permission_denied`; insufficient role remains `permission_denied`. The `F1-04-service-ipc-ci.md` file and the `ipc.rs` tests are to be aligned by the service workstream.
- The MCP adapter (F5) maps `unauthenticated` and `session_expired` to "a new authorization in the app is needed" and `permission_denied` to "action outside the grant".
- `docs/ARCHITECTURE.md` lists the public codes: `unauthenticated` must be added.
- The audit records the denial code: attempts with invalid credentials are distinguishable from policy denials.

## Required evidence

- Synthetic tests in `lightvm-proto` (`error_codes.rs`) and `vm-core` (`negatives.rs`, `scenarios.rs`): recorded in `docs/verification/F1-01-proto-core.md`.
- IPC service tests with the new code: to be recorded by the F1-04/F1-11 workstream.

## References

- `docs/ROADMAP.md`, sections 6.3, 7.3, 7.4, 9.
- ADR-0006 (MCP stdio, pairing and grant).

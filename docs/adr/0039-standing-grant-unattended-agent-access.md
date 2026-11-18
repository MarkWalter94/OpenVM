# ADR-0039 — Standing grant: unattended agent access to one VM (owner decision; declared exception to ROADMAP §4 and §7.3 and to ADR-0006)

- Status: Accepted (2026-11-18): implemented, verified in test and on hardware (`docs/verification/R6-10-standing-grant.md`)
- Date: 2026-11-18
- Owner: project owner (decisions D1–D3 below); implementation recorded below

## Context

An agent has to work for days on **one** dedicated VM (installing and testing a product) without the user approving a new session every few hours and without losing its authorization when LightVM restarts. The frozen roadmap says that MCP is off at every start and that *no active authorization survives a restart* (§4), that on client loss and expiry sessions are revoked *and stopped* through the emergency path and that resuming the agent needs a new authorization (§7.3); ADR-0006 repeats it. This ADR declares a bounded exception, on the model of ADR-0033 and ADR-0034. `docs/ROADMAP.md` is not changed.

## Decisions of the owner (2026-11-18)

| # | Decision |
| --- | --- |
| D1 | When a session born from a standing grant ends (grant expiry, lease expiry, agent disconnected), **the VM keeps running**. |
| D2 | A standing grant lasts at most **7 days**; then the user creates a new one in the app. |
| D3 | Development: local commits on a dedicated branch, no push. |

Design decisions confirmed with the owner during the implementation:

- Emergency Stop **revokes** the standing grant.
- One VM per grant, at most **one live** grant.
- A request that does not conform is **denied**, never queued for a manual approval.
- Instead of the global policy revision of the core (in memory, restarted at 1 at every start, never incremented in production), the grant records the **configuration revision of its VM** (`config_revision`, persistent in the catalog). A different revision **suspends** the grant until the user confirms it again in the app. A restore to a baseline or a snapshot with an older configuration changes the revision too, and suspends it the same way.
- The MCP adapter keeps asking for every capability and no VM (they are hints): a conforming request receives **grant ∩ requested** capabilities, never more than the grant.

## Decision

A **standing grant** is created by the user in the app (UI role only) for one VM, with the capabilities, Tools resources, network endpoints, per-session quotas, lease and input handover a session may receive, an optional expected client name, and a lifetime of 60 s .. 7 days (`MAX_STANDING_GRANT_S = 604 800`).

- **Persistence.** The catalog keeps the grant (policy, state, counters) and the **SHA-256** of its pairing credential, never the credential (migration 0012, table `standing_grants`, at most one live row by a partial unique index). Deleting the VM revokes its grant **in the same transaction**. At start the service loads the live grant: an expired one is marked `expired`; a live one has its credential hash registered again, with the lifetime left.
- **Pairing credential.** Issued once at creation (role `Pairing`, no VM permission by itself), expiring with the grant. The app writes it to the same pairing file as today (`%LOCALAPPDATA%\LightVM\pairing\pairing.json`, folder with a single ACE for the user). While the grant is live the file is **not** removed at service stop or start nor when the agent access switch is turned off, and the app rewrites its endpoint at every service start; it is removed at revocation, expiry and Emergency Stop. Revocation and expiry also close the connections already open with the credential (the handshake check alone would not).
- **Automatic approval.** A `request_session` presented with the standing credential is decided by the supervisor at once, never listed for the user, never approvable with a click. It is approved when: the requested VMs are none or exactly the grant's VM; the requested endpoints are within the grant's (none means all of the grant's); the grant ∩ requested capabilities are not empty; the declared client name matches when the grant sets one; the grant is active (not suspended) and not expired; no other session is live; the VM still has the grant's configuration revision (checked on the supervisor thread together with the session creation). Anything else is denied with `permission_denied` and a stable `detail.reason`. The session lasts `min(24 h, time left of the grant)`; the wire limits of a manual approval are unchanged. The reply says `standing: true`. With an ordinary pairing credential nothing changes: the switch is required and the user decides.
- **Audit.** The automatic approval is recorded with the actor `standing_grant { grant_id }` (session creation and request), never as `user`; creation, revocation, expiry, suspension and confirmation have their own actions. The app receives a `standing_grant` event for each change, automatic approval and denial.
- **End of session (D1).** For a session of a standing grant, `grant_expired`, `lease_expired` and `client_lost` end the session **without** stopping the VM. UI close (that is closing LightVM, where the VMs are stopped in an orderly way anyway), host resume after sleep and Emergency Stop still stop it. Sessions approved by the user keep ROADMAP §7.3 unchanged.
- **Agent access switch.** It concerns ordinary pairing only: turning it off ends the sessions approved by the user, not the grant's session. The grant is ended by **Revoke**, Emergency Stop, deleting the VM or its expiry.
- **Baselines.** Baselines created by sessions of a grant stay restorable by every later session of the **same** grant (also after a restart); the user's baselines stay restorable only as the initial baseline of a session; user snapshots stay out of reach.
- **MCP adapter.** Under a standing grant it requests a new session by itself when one ends by expiry or connection loss (service restart included), re-reading the pairing file, with increasing backoff and no tight loop; it stops on an explicit `session_end` or when the credential is refused. No tool creates or changes grants: the agent cannot grant itself anything.

Unchanged: one session at a time; quotas per session; audit; no NAT under an agent session; no access to user snapshots, host paths or QMP; peer check on the pipe; the `Pairing` role gains no request.

## Consequences

- A VM can run with **no agent connected**, without network except the granted endpoints, for up to 7 days; the app shows a fixed indicator on every page while a grant is live and warns at start.
- A long-lived credential sits in a file readable by processes of the same user (as the 10-minute one, for longer). Same-user processes are out of the threat model (ROADMAP §7.1); the file is removed when the grant ends.
- Sessions are approved without a click; every one is in the audit with the grant as actor and in the session reports.
- Revoking one session of a grant does not stop the agent: the adapter asks for the next one at once. The agent is stopped by revoking the grant.
- Session tokens still never survive a restart (§7.4 "Sessions" row): only the grant does, and a new session is created after the restart.

## Implementation

`lightvm-proto` (`standing.rs`, `ipc.rs`), `vm-storage` (migration 0012, `standing.rs`, `vm_delete.rs`), `vm-core` (`create_session_from_standing_grant`, `Session::stops_vm_on`, `terminate_user_approved_sessions`), `lightvm-service` (`standing.rs`, `server.rs`, `credential.rs` hashed entries, `agents.rs`, `engine.rs`, `supervisor.rs`, `adapters/catalog.rs`), `lightvm-mcp` (automatic renewal, `help`), `apps/desktop` (Agents page, indicator, pairing file).

## Required evidence

- Automated (`crates/lightvm-service/tests/standing_grant.rs`, `crates/vm-core/tests/scenarios.rs`, `crates/vm-storage/src/standing.rs`, MCP and UI tests): listed with their results in `docs/verification/R6-10-standing-grant.md`.
- Hardware: real QEMU cycle with at least three accelerated session renewals, a restart of LightVM in the middle, the VM still running, revocation. Recorded in `R6-10` with verified / failed / not executed.

## References

Roadmap §4, §7.1, §7.3, §7.4, §9 (unchanged); ADR-0006 (amended by this exception); ADR-0033, ADR-0034 (model of a declared exception); ADR-0026 (public codes).

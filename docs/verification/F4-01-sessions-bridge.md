# F4-01 — Bridge and sessions: agent sessions, gate, input ownership, emergency command (backend)

- Execution date (UTC): 2026-10-11
- Executed by: development agent "backend F4" (Claude Code session), F4 work split (the UI for sessions is a separate agent)
- Roadmap step: section 10 F4 ("Bridge e sessioni"): extension of the F1/F2 internal APIs (screenshot, mouse, keyboard, status, start/stop/reset) to agent sessions bound to VM, session and run; capabilities, expiries, quotas, allowlist, exclusive input control; pause, revocation, Emergency Stop and local emergency command; complete audit and containment; negative tests and stop-latency measurements. Sections 7.2 (point 7, 9, 10), 7.3, 7.4, 9.
- Related ADRs: ADR-0006, ADR-0007, ADR-0009, ADR-0026 (none modified; deviations listed at the end)
- Overall outcome: **partial** — every item of the exit criterion is **verified** on this host (Windows 11, WHPX) with the service and either the real core with fake backend (protected calls, allowlist, quotas, lifecycle, flood with a stuck supervisor and a full queue) or real QEMU (agent input driving the fixture, stop latencies with a flooding agent). Not executed: Linux, a real MCP client (F5), a user driving the sessions UI, host suspend/resume.

Allowed outcomes: **verified**, **failed**, **not executed** (section 17). Raw logs and PNG evidence in `lab/f4/` (`logs/run1.log`, `evidence-1/`), outside the repository.

## Purpose

1. A session lifecycle over the authenticated IPC: pairing connection → session request with verification code → approval in the app (one VM, capabilities, duration, quotas, lease) → session token delivered only to the waiting agent and **bound to its connection**.
2. Every agent call checked by the core on receipt and right before forwarding, plus a service **gate** for pause/revocation that does not depend on the supervisor queue.
3. Exclusive input ownership (user OR agent), explicit handover, instant takeback, key release on every change.
4. Pause, revocation, Emergency Stop, lifecycle rules of ROADMAP 7.3 (lease ≤ 10 s, expiry, client loss, UI loss → revoke and stop), agent access switch off at every start, local emergency command usable with the UI frozen.
5. Audit of every agent call; containment (rate limit per session, bounded queues, quotas, byte cap of screenshots, chunked transfer under the 64 KiB frame).
6. Exit criterion: protected calls verified by the core; no control of other VMs; revoke/stop with an uncooperative client and a full queue; input on a real VM.

It does not demonstrate: the MCP adapter and real LLM clients (F5), the sessions UI (other agent), reports (F5), Linux, host sleep/resume with a real suspend, tools `vm.snapshot`/`vm.restore`/`vm.logs` (capabilities exist, calls arrive in F5).

## Environment

| Item | Value |
| --- | --- |
| Host | Intel Core i9-14900K (32 logical processors), 63.8 GiB RAM (as F3-01) |
| OS | Microsoft Windows 11 Pro 10.0.26300 |
| Accelerator | WHPX |
| Toolchain | Rust 1.99.0 `x86_64-pc-windows-msvc`, wrapper `cargo-msvc.cmd`, `CARGO_TARGET_DIR=target\f4`; real test in `--release` |
| QEMU | 11.1.0 portable `lab/qemu/qemu-11.1.0/` (hashes in F3-01) |
| Guest | `lab/media/ubuntu-26.04.1-desktop-amd64.iso` (live session), fixture data ISO from `lightvm-fixture` (ADR-0019, as F3-01) |
| VM profile | `pc-q35-11.1`, `-cpu Skylake-Client-v4`, 4 vCPUs, 4096 MiB, 8 GiB empty qcow2, offline |
| Base commit | `1c38571` plus the uncommitted F4 changes of this file |

## What was implemented

### Session lifecycle (IPC contract, additive; protocol version 1 unchanged)

- **Agent access switch** (service): `get_agent_access`, `set_agent_access` (UI). **Off at every service start**; `request_session` refused while off (`permission_denied`, `reason=agent_access_disabled`); turning it off denies pending requests and revokes live sessions (VMs keep running). Audited (`agent_access_enable` / `agent_access_disable`), event `agent_access`.
- **Request** (pairing role): `request_session {client_name, purpose, requested_vms?, requested_capabilities?}` → `session_requested {request_id, verification_code "123-456", expires_in_s 300}`; event `session_request` to the UI; one pending request and one session per connection, 8 pending at most; texts truncated (64 / 512 chars) and stripped of control characters. `await_session {request_id}` (long, served detached) returns the grant, or `permission_denied reason=denied`, or `session_expired reason=request_expired`; a decision taken before the agent awaits is kept for it.
- **Approval** (UI): `list_session_requests`, `approve_session {request_id, approval: {vm_id, capabilities, duration_s 60..86400, quotas?, lease_ms 1000..10000?, give_input}}`, `deny_session`. The service generates the 32-byte token (OS RNG), the core issues grant and lease (`create_session`, MVP limit of **one live session**, `resource_limit` otherwise), the session is bound to the requesting connection, the agent receives `session_granted {session_id, token, grant, lease}`. The token is never logged (`Debug` redacted, never in audit, never sent to the UI). Quotas default to `AGENT_DEFAULT_QUOTAS`; `max_screenshot_bytes` capped at 8 MiB.
- **Agent calls** (`agent {session: {session_id, token}, call}`): `list_vms`, `vm_status`, `submit` (start, stop, force_stop, reset, input), `screenshot` (metadata; PNG kept by the engine, verified against the backend's SHA-256), `screenshot_chunk` (32 KiB raw, base64, fits the 64 KiB frame), `operation`, `renew_lease`, `end_session`. A start is served with a deferred reply so lease renewals keep flowing during the start checks.
- **UI control**: `list_sessions` (live first, then the last 16 terminated, with usage), `pause_session`, `resume_session`, `revoke_session` (closes the agent connection, VM not stopped, returns the `RevocationReport`), `set_input_owner`. Events `session_changed`, `session_request_closed`, `input_owner`.
- **Emergency command**: second endpoint (same per-user ACL as the IPC endpoint, random name) published in `<catalog>/emergency.endpoint`; accepts only `{"type":"emergency_stop"}` (256-byte frame, 5 s timeout) and runs the same priority path as the UI's Emergency Stop, without credential and without the supervisor thread. `lightvm-service --emergency-stop --catalog <dir>` prints the report; exit 0 completed, 5 not completed, 6 no service. The file is removed at shutdown. Rationale: the only effect is fail-safe (stop everything), it must work when the UI that holds the credential is frozen; a same-user process can already kill the processes (threat model 7.1).
- Contract text for the UI agent: scratchpad `f4-ipc-contract.md` (final).

### Checks and priority path

- **Core (vm-core)**: per-VM **input owner** (`set_input_owner`, `input_owner`); input from a non-owner refused on receipt (`permission_denied`, `reason=input_owned_by_agent` for the user, `input_not_owned` for an agent) and discarded right before forwarding; every change (and the end of the owning session) discards queued input of the previous owner and records the run whose pressed keys must be released (`take_input_releases`, executed by the engine with `release_all_input`). Agent `list_vms`/`vm_status`, screenshot reads (`read_screenshot`) and lease renewals are audited; `terminate_session_with_stop` does not stop the VMs of an already terminated session; `terminate_all_sessions`; `finish_emergency_stop`; `record_event`.
- **Gate (service, `agents.rs`)**: pause, revocation, Emergency Stop, UI loss, agent access off and client loss flip the gate synchronously in the server task (the **point of revocation**), close the agent connection (its in-flight and queued requests are abandoned) and queue the change for the core. The gate is checked on receipt and again by the supervisor thread right before executing; the engine **applies the queued changes to the core before every admission and before every forwarding** (`SessionGate`), so a request already past the gate and waiting inside the supervisor still meets the core's refusal.
- **UI control without queue slot**: approval, pause, resume, revocation, input owner and agent access off go to the supervisor as `Command::Control` (no semaphore permit): a full queue never prevents the user from stopping an agent.
- **Lifecycle (ROADMAP 7.3)**: grant and lease expiry checked every second on the supervisor thread (`tick_sessions`) → session terminated and its running VM Emergency-Stopped with OS primitives (`force_stop`), connection closed; agent connection lost → `client_lost` + stop; UI lost → `ui_closed` + stop (also with `keep_vms`); Emergency Stop → every session `emergency_stop`; service shutdown → sessions end (memory only, nothing survives a restart).
- **Containment**: agent connections have their own rate limit (burst 20, 50 requests/s); quotas per session (operations, input events, screenshots, bytes per screenshot, queued operations); bounded supervisor queue (64) and per-connection sequential serving; screenshots bounded and transferred in chunks; QMP operations bounded by the backend timeouts (no blind retry of input).

### Code

| Crate | Changes |
| --- | --- |
| `lightvm-proto` | IPC: 13 requests, `AgentCall`, `SessionApproval`, `SessionAuth`, `InputOwner`, `SessionInfo`, `SessionRequestInfo`, `AgentVmView`, `SessionUsage`, `SessionRequestOutcome`, `EmergencyCommand`, constants, base64 helpers; 17 responses; 5 events; 9 audit actions; schemas regenerated (+ `ipc_emergency_command.json`) |
| `vm-core` | input ownership with two checks and key-release bookkeeping, audit of agent observations/lease/screenshot reads, `terminate_all_sessions`, `finish_emergency_stop`, `record_event`, no double stop; tests `f4_sessions.rs` (8) |
| `lightvm-service` | `agents.rs` (switch, requests, gate, binding), engine session API (`create_session`, `agent_call`, `end_session`, `tick_sessions`, `set_input_owner`, `session_changes`, `input_owner_changes`, gate sync), backend `take_screenshot` (real and fake), supervisor (gate re-check, control commands, expiry tick, events), server (dispatch, priority path, emergency endpoint), client (`read_emergency_endpoint`, `emergency_stop`, `into_parts`), main (`--emergency-stop`); tests `f4.rs` (13), `f4_real.rs` (1, ignored) |
| `apps/desktop` | none (no struct changed) |

## Results

| Item | Outcome | Evidence |
| --- | --- | --- |
| Every protected call verified by the core on receipt: 6 actions × 10 caller conditions (user; agent with all capabilities; without capability; other VM; paused; expired; revoked; wrong token; no token; not input owner), expected code, no backend effect, `denied` audited with the code | verified | `vm-core/tests/f4_sessions.rs::every_protected_call_is_checked_on_receipt_for_every_caller` (60 cases), `observation_calls_are_checked_for_every_caller` |
| Second check right before forwarding (pause, revoke, expiry, input taken back between receipt and forwarding → discarded, no effect) | verified | `second_check_before_forwarding_discards_without_effect` |
| State guards apply to agents (action × state) | verified | `authorized_agent_calls_follow_the_state_guards` |
| Request × role over IPC: UI-only requests refused to pairing, agent-only requests refused to the UI, no effect | verified | `lightvm-service/tests/f4.rs::request_role_matrix` (33 cases) |
| Session token bound to the connection; wrong token; UI role cannot make agent calls; one live session | verified | `f4.rs::session_is_bound_to_its_connection_and_role` |
| Allowlist: list filtered, status/start of another VM refused (`permission_denied`, no data) | verified | `f4.rs::session_lifecycle_through_the_bridge`, core matrix (`AgentOtherVm`) |
| Quotas: screenshots, input events, operations; screenshot byte cap | verified | `f4.rs::quotas_and_screenshot_byte_cap_are_enforced` |
| Agent access off at start, gating, denial/expiry of requests | verified | `f4.rs::agent_access_is_off_at_start_and_gates_session_requests`, `denied_and_expired_requests` |
| Pause (mutations refused, observation allowed), resume, revocation closes the connection, token useless afterwards, VM not stopped | verified (fake backend and real QEMU) | `f4.rs::session_lifecycle_through_the_bridge`; `f4_real.rs` |
| Lease expiry → session terminated, VM stopped, connection closed (lease 1000 ms: closed 1.49 s after the last renewal); renewals keep it alive | verified | `f4.rs::lease_expiry_terminates_the_session_and_stops_the_vm` |
| Client lost → `client_lost`, VM stopped; UI lost (even `keep_vms`) → `ui_closed`, VM stopped | verified | `f4.rs::client_loss_…`, `ui_loss_terminates_the_sessions_and_stops_their_vms` |
| Session never survives a restart; switch off again | verified | `f4.rs::restart_invalidates_sessions_and_turns_agent_access_off` |
| Emergency command: endpoint published, one verb, works without UI connection, revokes sessions; binary `--emergency-stop` (exit 0 with a running service, 6 without) | verified | `f4.rs::emergency_endpoint_stops_everything`; manual run of `target\f4\debug\lightvm-service.exe` (this session) |
| Input ownership: user and agent never inject together; handover and takeback release keys | verified (fake: release call counted; real QEMU: refusals on both sides, takeback after a held `shift`) | `f4_sessions.rs::input_ownership_is_exclusive_and_handover_releases_keys`; `f4.rs::session_lifecycle_through_the_bridge`; `f4_real.rs` |
| Screenshot chunks reassembled, SHA-256 equal to the metadata | verified (fake 100 000 bytes; real 1280×800 PNGs, 465 chunk reads) | `f4.rs`, `f4_real.rs` |
| Audit: every agent call (action, ids, outcome, order), no token, no typed text | verified | `f4_sessions.rs::agent_calls_are_audited_in_order`; `f4.rs` (token absent from the audit JSON); `f4_real.rs` (5552 events, `fx.sh` absent from the audit file) |
| **Agent input on a real VM**: the agent starts the VM, waits for the desktop on its own screenshots, opens a terminal and runs the fixture; pixel-exact check (ADR-0019) on agent screenshots: `mostra` → absent (V3), `scrivi` → present (V4), `mostra` → present (V5) | verified (1 run, 0 retries) | `f4_real.rs`, `lab/f4/logs/run1.log`, `lab/f4/evidence-1/*.png` |
| Revoke and Emergency Stop with a non-cooperative client flooding requests and a full queue | verified | measurements below |

### Test counts (this session, all green)

`lightvm-proto` 36, `vm-core` 79 (of which `f4_sessions` 8), `lightvm-service` lib 51 + `ipc` 24 + `core_path` 6 + `f2` 6 + `f3` 5 + `f4` 13; `f4_real` 1 (ignored, run in release: passed in 254 s). `cargo clippy --workspace --all-targets -D warnings` and `-p lightvm-service --no-default-features` clean; `cargo fmt --check` on the three crates.

## Measurements: stop latency with a flooding agent

Definitions: t0 = instant the UI sends the command. (a) **refusal of new requests**: the agent connection receives `closing` (gate closed, connection abandoned; for the pause: first `session paused` refusal). (b) **termination of VM processes**: Emergency Stop reply, sent only after the end of every QEMU process was observed. The agent pipelines input requests without waiting for replies (2 ms apart, above the 50/s rate limit). "Forwarded after" counts agent operations audited `forwarded` after t0 (≤ 1 allowed: the one already inside the backend) and after the core's `session_terminate` (must be 0).

### Real QEMU (`f4_real.rs`, `LIGHTVM_F4_SAMPLES=10`, live Ubuntu desktop running, flood of `shift` press/release)

| Measure | n | p50 | p95 | max |
| --- | --- | --- | --- | --- |
| revoke → agent connection closed | 10 | 0.0 ms | 0.1 ms | 0.1 ms |
| revoke → UI reply (core revocation recorded) | 10 | 3.6 ms | 5.2 ms | 5.2 ms |
| Emergency Stop → agent connection closed | 10 | 8.2 ms | 9.7 ms | 9.7 ms |
| Emergency Stop → reply with the end of the QEMU process observed | 10 | 62.7 ms | 298.2 ms | 298.2 ms |

Half the Emergency Stops came from the UI request, half from the local emergency endpoint. Every sample: 0 agent operations forwarded after the termination, at most 1 after the command, `completed: true`, 1 process terminated, VM `stopped`; revocations left the VM running. The flood was served at the rate limit (one `resource_limit` reply observed: the service consumed requests at about the admitted rate, 1549 inputs forwarded over the run). The "0.0 ms" of the revocation means the `closing` frame reached the agent before the UI's own reply; resolution of `Instant`.

### Fake backend (`f4.rs`, input 5 ms each, 20 samples split revoke / pause / Emergency Stop)

| Measure | n | p50 | p95 | max |
| --- | --- | --- | --- | --- |
| revoke → agent connection closed | 7 | 0.07 ms | 0.21 ms | 0.21 ms |
| revoke → UI reply | 7 | 0.27 ms | 0.38 ms | 0.38 ms |
| pause → first `session paused` refusal | 7 | 0.22 ms | 0.25 ms | 0.25 ms |
| Emergency Stop → agent connection closed | 6 | 0.07 ms | 0.17 ms | 0.17 ms |
| Emergency Stop → reply | 6 | 0.30 ms | 1.61 ms | 1.61 ms |

888 replies to the flood, 245 rate-limited, 0 operations forwarded after a command.

### Stuck supervisor and full queue (`f4.rs::revoke_and_emergency_stop_with_a_stuck_supervisor_and_a_full_queue`)

The fake backend blocks the supervisor thread inside the agent's first input; four other pairing connections fill the supervisor queue (capacity 4, at least one `supervisor queue full` observed). Revocation (new UI connection) and Emergency Stop (emergency endpoint) close the agent connection in **5.2 ms** and **5.3 ms** (connection set-up included) while the supervisor is still stuck; when it resumes, the request that was stuck inside it meets the core's refusal: **0 agent inputs forwarded**, 0 forwarded after the termination; the UI's revocation reply arrives after the supervisor resumes (control command without queue slot).

Values are from one host and one run each; they are measurements, not guarantees (ADR-0009 target: p95 ≤ 2 s for the end of the QEMU process — met here).

## Negative tests (ROADMAP 7.4, session-related rows)

| Row | Attempt | Outcome | Evidence |
| --- | --- | --- | --- |
| Sessions | token missing, wrong, revoked, expired, reused after a restart; token used on another connection | `unauthenticated` / `session_expired`, no effect | core matrix, `f4.rs` (bound, restart) |
| VM | session on A asks status/start/screenshot/input on B | `permission_denied`, no data of B | core matrix, `f4.rs` |
| Policy | agent tries UI-only requests (approve, set agent access, set input owner, pause, revoke, create VM, …) | `permission_denied`, no effect (an agent has no request that changes policy) | `f4.rs::request_role_matrix` |
| Revocation | input flood during revocation / Emergency Stop, with a full queue and a stuck supervisor | no forwarding after the revocation point; in-flight operations identified (`RevocationReport.already_forwarded`); end of processes verified | measurements above |
| Resources | screenshot flood, oversized screenshot, quota exhaustion, rate | `resource_limit` | `f4.rs::quotas_…`, flood tests |
| Lifecycle | client lost, UI lost, service restarted, lease not renewed | session terminated, VM stopped (client/UI/lease), nothing survives a restart | `f4.rs` |
| Lifecycle | host resumed from sleep | **not executed** (a resume shows as a lease expiry: the UTC clock passes the lease; no real suspend tested) | — |

## Deviations and decisions to record

- D-1: **Gate in the service** in addition to the core's two checks: the user's pause/revocation take effect before the supervisor queue; the engine pulls gate changes into the core before every admission and forwarding. The core remains the authority (it refuses on its own state); the gate only makes it earlier. Proposed for ARCHITECTURE/ADR-0009 notes.
- D-2: **Emergency endpoint without credential** (only verb: Emergency Stop, same-user ACL). To be recorded in ADR-0009 ("local emergency command") and SECURITY.md.
- D-3: **Revocation keeps the pairing credential** (the connection is closed, a new session needs a new approval); the Emergency Stop and UI loss still revoke every non-UI credential (F1 behaviour).
- D-4: **Lease expiry, client loss and UI loss stop the session's VM** (ROADMAP 7.3 literally); an agent's own `end_session` and the user's revocation do not.
- D-5: **Agent screenshots in chunks** (32 KiB, base64) rather than a larger frame; F5's MCP adapter reassembles them into image content.
- D-6: Input ownership defaults to the user; the agent gets it only by explicit handover (`give_input` at approval or `set_input_owner`); one live session in the MVP (`ResourceLimits::MVP_DEFAULT.max_agent_sessions = 1`).
- Docs not updated by this agent (ownership): `docs/verification/README.md` index row, `docs/ARCHITECTURE.md`, `docs/SECURITY.md`, ADR-0009, `docs/STATUS.md`.

## Limits

- One host, Windows only; Linux not executed (the emergency endpoint and the gate are platform-independent code, the transport is the F1 one).
- The real stop measurements flood with keyboard events (pointer events need the current geometry); the real input path for pointer events was verified in F2/F3.
- Key release on handover was observed directly only on the fake backend (call counted); with real QEMU the handover path runs `release_all_input` (F2) and both refusals were verified, the absence of a stuck key in the guest was not inspected visually.
- `vm.snapshot`, `vm.restore`, `vm.logs` capabilities can be granted but have no agent call yet (F5).
- Pending session requests are kept in memory and expire after 300 s; a request expires only when listed, awaited or a new one arrives (no timer), which is invisible to the agent.

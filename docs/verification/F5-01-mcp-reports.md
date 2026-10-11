# F5-01 — MCP stdio adapter, session reports, real clients (backend)

- Execution date (UTC): 2026-10-11
- Executed by: development agent "backend F5" (Claude Code session); F5 work split (the sessions UI is a separate agent)
- Roadmap step: section 10 F5 ("MCP e report"): stdio adapter, pairing/local authorization, handshake and tool schemas; at least two real clients with documented versions; JSON/Markdown reports with distinct outcome states; complete scenario (control of an app in the guest, report, revocation, restore). Sections 7.3, 7.4, 9, 11 (points 7–12).
- Related ADRs: ADR-0006, ADR-0019, ADR-0021 (none modified here; deviations at the end)
- Overall outcome: **partial** — adapter, reports, protocol and report tests **verified**; complete scenario with real QEMU and the real adapter **verified with a scripted (non-LLM) MCP client**; **no real LLM client run** on this date: Claude Code 2.1.220 **not executed** (the CLI was logged out), pi 0.85.0 **not executed** (no native MCP support). The F5 exit criterion is **not met** (see the last section).

Allowed outcomes: **verified**, **failed**, **not executed** (section 17). Raw logs, transcripts, screenshots and exported reports in `lab/f5/` (outside the repository).

## What was implemented

### MCP adapter (`crates/lightvm-mcp`, binary `lightvm-mcp`)

- `rmcp` 3.5.1 (ADR-0021), stdio, **MCP 2025-11-25 only** (`supported_protocol_versions` restricted; a client offering another revision receives 2025-11-25); tools capability only; `ServerHandler` implemented by hand (`list_tools`/`call_tool`) so every result is checked before it leaves.
- **Pairing** (contract with the app, `f5-contract.md` §1): the adapter takes **no argument** (only `--version`/`--help`); it reads the app's pairing file (`%LOCALAPPDATA%\LightVM\pairing\pairing.json`, or the file named by `LIGHTVM_PAIRING_FILE`, a path), refuses it unless the folder is private (Windows: `icacls` lists exactly one entry, the current user from `whoami`; Unix: no group/other bits), regular, ≤ 4 KiB, format `lightvm-pairing-v1`, not expired. Errors say `Pair a client in LightVM -> Agents first`; the content is never printed or logged (`Debug` redacted, buffer zeroed).
- **No action at start**: no IPC connection until `session_request`; never starts a VM nor turns agent access on (test: only the UI connection exists after `initialize` + `tools/list`; with access off the request is refused with `agent_access_disabled`).
- **Session**: `session_request {purpose}` → `request_session` (client name = MCP `clientInfo` + "via lightvm-mcp"), the **verification code** is returned in the tool result; `session_status {wait_s ≤ 50}` waits for the decision (background `await_session`); lease renewed every 3 s on a multiplexed connection (long calls never block renewals); `session_end`, `session_declare {claimed_outcome, summary}` (claim only).
- **Catalogue v0.2** (tool names with `_` — `vm.list` → `vm_list` — because model APIs refuse `.`; titles carry the catalogue name): `vm_list`, `vm_status`, `vm_start`, `vm_stop` (ACPI or `force`), `vm_reset`, `vm_screenshot` (MCP **image content** + structured metadata VM/run/operation/display/image size/SHA-256 of the returned and of the full PNG; `scale: half` optional; 4 MiB cap plus the grant's byte quota), `vm_mouse_move`, `vm_mouse_click` (display pixels; stale geometry retried once), `vm_keyboard_type` (US layout, printable ASCII + `\n` `\t`, ≤ 1024 chars, refused **before** typing anything otherwise; 30 ms per character), `vm_keyboard_press` (allowlisted QEMU key names, chords `ctrl+alt+t`, repeat ≤ 20), `vm_snapshot`, `vm_restore`, `vm_baselines`, `vm_logs`, `operation_get`, `report_get`.
- Results: structured content generated from Rust types, **validated against the published output schema** before returning (small validator, `schema.rs`); operational errors are tool results with `isError: true` and `{code, message, retryable, details}` (no structured content); invalid/unknown arguments → `isError` `invalid_argument` (2025-11-25 rule); unknown tool → JSON-RPC `-32602`.
- Containment: adapter rate limit (burst 20, 10 tool calls/s) on top of the service's (50 req/s per agent); timeouts per call (30 s; start/snapshot/restore 30 min), never retried blindly (a timeout tells the model to check `vm_status`/`operation_get`); reads refused by the service's rate limit are retried, mutations never.
- No host path, QMP, VM create/delete, media or policy tool; identifiers are parsed as UUIDs (paths, UNC, device names, option strings → `invalid_argument`); unknown argument names refused (`deny_unknown_fields`).
- stdout carries only MCP; diagnostics on stderr (`lightvm-mcp: …`), without credential, token or typed text (checked in tests).

### Service and core

- **Agent calls** (additive IPC): `list_baselines`, `snapshot` (`vm.snapshot`, VM `stopped`, always a **new** baseline labelled `agent: …`, an unclean last stop is refused — no hidden decision), `restore` (`vm.restore`, VM `stopped`, only the session's **initial baseline** or a baseline **created by the session**, else `permission_denied reason=baseline_not_restorable`), `logs` (`vm.logs`: report events + last 100 lines of the run's QEMU stderr with host paths replaced by `<path>`), `declare`, `tool_note` (bounded adapter notes; `vm_keyboard_type` records `{chars, sha256, layout}` only), `report` (own session only). Snapshot/restore are long jobs served detached, so renewals keep flowing.
- **Core**: agents may create/restore baselines with the explicit capability on their grant's VM (checked by `authorize`, then the stopped/busy checks); deletion stays user-only. An agent's own restore does **not** end its session (other live sessions on the VM still end with `restored`) — deviation D-2.
- **Reports** (`lightvm-service/src/reports.rs`, types in `lightvm-proto/src/report.rs`): one report per session, opened at the approval, written under `<catalog>/sessions/<session_id>/` (`report.json` canonical, `summary.json`, `shots/NNNN.png`), survives restarts. Content: session/VM/run ids, baseline id + label + disk/firmware SHA-256, guest OS, VM manifest SHA-256 (canonical JSON of the configuration) and revision, policy revision, capabilities, quotas, expiry, versions (service, IPC, MCP revision declared by the adapter, backend incl. QEMU version, accelerator), objective, ordered events with UTC timestamps (every agent call with outcome and error code — refusals included — lifecycle, VM states, screenshots by hash, adapter notes), errors by code, agent declaration, verdict history, retention counters, `report_sha256` (tamper detection only). Every agent call is recorded by the connection right before its reply, so the record precedes any later request of the same agent.
- **Outcomes**: `passed | failed | inconclusive | cancelled | timed_out | infrastructure_error`, source `pending | termination | verifier | user`. Default from the termination cause (never `passed`); automatic **ADR-0019 pixel verifier** on the last stored full PNG (`verify_session`); explicit **user judgement** (`set_session_verdict`, attributed to "local user", rationale required); a report left live by a crashed service becomes `supervisor_lost` → `infrastructure_error` at the next start. The agent's declaration never sets the outcome.
- **UI requests** (UI role only; final shapes in `f5-contract.md` §4–5): `list_session_reports`, `get_session_report` (header), `get_session_report_events` (pages of ≤ 50, frame limit 64 KiB), `get_report_screenshot_chunk`, `set_session_verdict`, `verify_session`, `export_session_report` (JSON or Markdown to an absolute path chosen by the user; no UNC/device/`..`/reserved names/links; no overwrite without the flag; optional screenshots next to it), `delete_session_report` (terminated only). Event `session_report_changed`; audit `session_verdict`, `report_export`, `report_delete`.
- **Markdown**: every text that comes from the agent, the client or the guest is inside a code span or fenced block (control characters and backticks removed): no markup, no link, no terminal sequence (ROADMAP 7.4 "Report" row).
- **Retention**: ≤ 300 screenshots and 512 MiB per session (then hash only), ≤ 20 000 events, ≤ 100 reports (oldest terminated pruned).

## Results

### Automated tests (this session, all green; Windows 11, `CARGO_TARGET_DIR=target\f5`, debug)

| Suite | Count | What it shows |
| --- | --- | --- |
| `lightvm-mcp` lib | 9 | baseline-only negotiation; catalogue names/schemas, no path/QMP parameter, no create/delete/media tool; pairing file parsing and redaction; **folder privacy** (inherited ACL refused, restricted accepted); US keymap covers 0x20–0x7E and nothing else; chords; schema validator; half-size PNG |
| `lightvm-mcp/tests/stdio.rs` (real adapter binary over stdio, real service, real IPC, SQLite catalog, fake VM backend + fake image tool) | 6 | see the table below |
| `lightvm-service/tests/f5_reports.rs` | 3 | one report per outcome state and source (below) |
| `lightvm-service` lib (incl. `reports` unit tests: default outcomes, plain text, bounded notes, Markdown escaping, path stripping, destination validation) | 57 | |
| `lightvm-service` `f4` (role matrix extended with the 8 report requests: refused to pairing), `ipc`, `core_path`, `f2`, `f3` | 13 / 24 / 6 / 6 / 5 | no regression |
| `vm-core` (incl. `f3_baselines` updated for agent snapshot/restore) | 79 | |
| `lightvm-proto` (schemas regenerated, + `session_report.json`) | 36 | |

`cargo clippy -D warnings --all-targets` clean on `lightvm-mcp`, `lightvm-service` (default and `--no-default-features`), `lightvm-proto`, `vm-core`, `lightvm-fixture`; `cargo fmt --check` on the same crates.

### Protocol tests over stdio (`stdio.rs`)

| Test | Outcome |
| --- | --- |
| Handshake 2025-11-25; older revision answered with 2025-11-25; `tools/list` with input and output schemas; unknown tool → `-32602`; invalid arguments → `isError invalid_argument`; no session → `no_session`; no pairing file → `pairing_unavailable` | verified |
| Adapter connects to nothing before `session_request`; agent access off → refused, not turned on; no VM started; pairing file in a non-private folder refused (`not private … Agents first`); credential absent from stderr | verified |
| Every tool through an approved session: list, status, start, screenshot (image + metadata, full and half), mouse move/click (out-of-display refused), keyboard type (non-US character and other layouts refused), press (unknown key refused), logs (QEMU lines with `<path>`, paging by `after_seq`), operation lookup (other id → `not_found`), reset, snapshot (running VM → `state_conflict`; stopped → new baseline), baselines (initial and own checkpoint restorable), restore of own checkpoint and of the initial baseline (session survives), restore of a user's other baseline → `permission_denied baseline_not_restorable`, declaration (outcome stays pending), report (other session → `not_found`), end; report: every call recorded, keyboard note = length + SHA-256, **typed secret absent** from report, audit, stderr; no host path in `report.json`; MCP revision recorded | verified |
| Wrong VM (status/start/screenshot/baselines on a VM outside the grant → `permission_denied`, no data); host paths/UNC/device/traversal/option strings as ids → `invalid_argument`; smuggled `path` argument refused; capabilities not granted (reset, logs, snapshot, input) → `permission_denied`; input not handed over → refused, **no input forwarded** | verified |
| Pause (mutations refused "session paused", screenshot allowed), resume, lease kept alive past 10 s by the adapter, revocation → next call `session_closed`, status `closed`, report `cancelled`; a new request is possible | verified |
| Oversized screenshot (grant cap 1 000 bytes) → `resource_limit`; 40 pipelined calls → ≥ 10 refused by the adapter's rate limit | verified |

One unexplained `session_closed` was observed once during development in the full-tool test (before a test fix; not reproduced in the following 5 runs of the suite). Not investigated further: recorded as an open point.

### Reports (`f5_reports.rs`)

| State | How produced | Outcome |
| --- | --- | --- |
| `inconclusive` (termination) | agent `end_session` after declaring `passed` | verified — the claim did not set the outcome |
| `passed` (verifier) | ADR-0019 verifier, expected marker present, synthetic fixture frame | verified |
| `failed` (verifier) | same frame, expected absent; and a frame without fixture | verified |
| `passed` (user) | `set_session_verdict` with rationale; blank rationale refused | verified (4 verdicts in history) |
| `infrastructure_error` (verifier) | stored PNG altered after the fact | verified |
| `cancelled` (termination) | user revocation; Emergency Stop | verified |
| `timed_out` (termination) | lease of 1 s not renewed | verified |
| `inconclusive` (verifier) | no screenshot in the session | verified |
| `infrastructure_error` (termination) | report left `active` by a lost service, closed at restart as `supervisor_lost` | verified |
| Export JSON (canonical, hash), Markdown (escaped texts), screenshots; existing file without overwrite → `state_conflict`; relative/UNC/missing-folder destinations → `invalid_argument`; screenshot chunks; deletion (live refused, terminated removed); history newest first; event pages in seq and time order | | verified |

### Complete scenario with real QEMU — scripted client (`tests/f5_real_client.rs`, `LIGHTVM_F5_CLIENT=scripted`, run `scripted-1`)

**Not an LLM**: a deterministic MCP client in the harness speaks JSON-RPC to the real `lightvm-mcp` binary over stdio with the fixed fixture steps. It verifies the infrastructure chain, not a model.

Environment: Intel Core i9-14900K, Windows 11 Pro 10.0.26300, WHPX; QEMU 11.1.0 portable `lab/qemu/qemu-11.1.0` (hashes in F3-01); guest `lab/media/ubuntu-26.04.1-desktop-amd64.iso` SHA-256 `601e30fb…1bda1f`, live session; fixture ISO from `lightvm-fixture` (F3-01); VM `pc-q35-11.1`, `-cpu Skylake-Client-v4`, 4 vCPUs, 4096 MiB, 8 GiB empty data disk, offline; debug build of service and adapter.

| Step (UTC 2026-10-11) | Result |
| --- | --- |
| Baseline of the empty disk (first start, forced stop in the firmware → `allow_unclean`, recorded *not verified*), restore onto an overlay | baseline `01a12961-cb7d…`, disk SHA-256 `e80c9453…3014d2` |
| Boot, A0 desktop wait by a harness agent session | desktop after 127 s; reference screenshot without fixture colours |
| Adapter start, `session_request` → code `411-408`; **approval by the harness through the UI-role IPC (stand-in for the user's click in the app)**: VM, `vm.list/status/screenshot/input/logs`, 30 min, input handed over | approved 05:17:51 |
| Client: `vm_list`, `vm_screenshot(half)`, `vm_keyboard_press ctrl+alt+t`, screenshot, `vm_keyboard_type` (fixture `scrivi` command), full screenshot, `vm_logs`, `report_get`, `session_declare(inconclusive)` | all succeeded; QEMU log lines without host paths |
| Harness revokes after the declaration (05:18:12); client's next `vm_list` → `isError session_closed` | verified |
| `verify_session` (fixture, marker present) on the last stored full PNG | **passed** (V3 band + B5 present, 1280×800), source verifier; report: termination `revoked`, 3 screenshots, 99 events |
| Export JSON + Markdown + screenshots | `lab/f5/evidence/scripted-1/`; no host path in either file |
| Forced stop, **restore** of the baseline (81 ms), new boot, `fx.sh mostra` by the harness | **V6 marker absent** — verified |
| Audit | 1 159 events; typed command absent from the audit |

Test duration 727 s (debug build: most of it is the SHA-256 of the 6 GiB ISO at import and at each start).

### Real LLM clients

| Client | Version | Date | Outcome | Notes |
| --- | --- | --- | --- | --- |
| Claude Code CLI | 2.1.220 | 2026-10-11 | **not executed** | Prepared: `tests/f5_real_client.rs` with `LIGHTVM_F5_CLIENT=claude` runs `claude -p <prompt> --mcp-config <cfg> --strict-mcp-config --tools "" --allowedTools mcp__lightvm --permission-mode dontAsk --no-session-persistence --output-format stream-json --verbose --max-budget-usd 4` (prompt in the test, ~150 words: request session, screenshot, ctrl+alt+t, type the fixture command, screenshot, declare). Before spending a run, a one-line probe `claude -p "Reply with the single word OK." --tools ""` failed with `Failed to authenticate: OAuth session expired and could not be refreshed`; `claude auth status` → `loggedIn: false`. Logging in needs the user (`claude auth login`). No LLM run was made; 0 of the allowed 2–3 runs used. |
| pi coding agent | 0.85.0 | 2026-10-11 | **not executed** | `pi --help`: no MCP client built in (tools from built-ins and extensions); an MCP bridge would be a third-party extension (`pi install …`), not installed (download not authorized in this session). No configuration published (`examples/mcp/pi/README.md`). |

Example configurations: `examples/mcp/README.md`, `examples/mcp/claude-code/mcp.json`, `mcp-custom-pairing-file.json` (no credential; the adapter has no argument).

## Deviations and decisions to record

- D-1: **MCP tool names with `_`** (`vm_list`…) instead of the roadmap's `vm.list` (Claude API tool names allow `[A-Za-z0-9_-]` only); catalogue name in each title. To record in ADR-0021.
- D-2: **An agent's own `vm.restore` keeps its session** (ROADMAP 8 "il ripristino chiude la sessione di controllo" is kept for the other sessions of the VM); otherwise the snapshot/restore cycle "within the session" (ROADMAP 9) is impossible.
- D-3: **Pairing credential in a user-private file** written by the app (no OS credential store: the app forbids `unsafe`); the adapter checks the folder ACL before reading. Recorded by the coordinator in ADR-0006/SECURITY.
- D-4: `lightvm-mcp` becomes the **product adapter** (ADR-0021 foresaw a new `mcp-server` crate); binary renamed from `lightvm-mcp-spike` to `lightvm-mcp`; the spike's fake tools are gone.
- D-5: Extra read-only tools beyond the catalogue: `session_*`, `vm_baselines` (needed to know restorable ids), `operation_get`/`report_get` (the "lookup by id" of ROADMAP 9).
- D-6: Screenshot `scale: half` (2×2 box average, re-encoded PNG) is offered to the model only; the report and the verifier always use the backend's full PNG.
- D-7: The scripted harness creates the fixture baseline after a forced stop in the firmware (`clean_shutdown: false`), not after a guest shutdown as F3 did; acceptable for an empty data disk, recorded as such in the report.
- Docs not updated by this agent: `docs/verification/README.md` index row, `docs/STATUS.md`, `docs/ARCHITECTURE.md`, ADR-0021 status of the clients.

## What is missing for the F5 exit criterion

1. **Two real LLM clients with a successful path each**: 0 executed. Claude Code needs the user to log in (`claude auth login`), then `LIGHTVM_F5_CLIENT=claude LIGHTVM_F5_CLIENT_EXE=<claude.exe> LIGHTVM_E2E_QEMU_DIR=… cargo test -p lightvm-mcp --test f5_real_client -- --ignored --nocapture`. A **second client** with native MCP stdio support must be chosen (pi has none; candidates: Claude Desktop, VS Code/Copilot agent mode, Codex CLI, Gemini CLI — none tested here) or an MCP extension for pi approved and tested.
2. **Ten fixture runs completed and restored without infrastructure errors**: 1 run here (scripted client, passed + V6). The harness repeats per run; ten runs (≈ 12 min each in debug, less in release) with all attempts recorded, failures included, are still to do — with the real clients for the client part.
3. Reports "correct also for failure, timeout and inconclusive": verified with the fake backend (`f5_reports.rs`); with real QEMU only the `passed`/`cancelled` path of the scripted run.
4. Sessions UI (other agent) driving approval, verdict and export by a real user: not executed here.
5. Linux: not executed.

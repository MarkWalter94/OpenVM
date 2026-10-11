# F4-02 — Sessions UI of the desktop (agent access, pairing, approval, indicator, input control)

- Execution date (UTC): 2026-10-11
- Executed by: development agent "UI F4" (Claude Code session), F4 work split (backend in `F4-01`; the F5 agent works on the MCP adapter and the reports at the same time)
- Roadmap step: section 10 F4 (sessions bridge: the user's side of pause, revocation, Emergency Stop and exclusive input control) and the UI part of F5 that does not depend on the MCP adapter: "Schermata sessione con obiettivo, azioni, limiti e artefatti", "indicatore persistente della VM controllata e Emergency Stop sempre raggiungibile durante la sessione. Permessi approvati nell'app, senza pulsanti guest capaci di alterare policy"; sections 7.3 (lifecycle), 9 (session approval: VM, tools, duration, quotas)
- Related ADRs: ADR-0006 (pairing, limited grant, credentials never in arguments or logs), ADR-0009 (Emergency Stop), ADR-0022 (UI tests), ADR-0026 (error codes); none modified
- Overall outcome: **partial** — every flow is **verified** headless with the stand-in supervisor (kittests, snapshots) and on the real IPC framing (fake server), and **verified** end to end once with the real supervisor (`HEAD` = `99e62f2`), real QEMU 11.1.0 and a scripted agent client speaking the real IPC (run 1 below). **Not executed**: a real MCP client (the adapter is F5), a human at the physical screen, Linux, an agent driving an installed guest through the UI-approved session (done by `F4-01` without the UI).

Allowed outcomes: **verified**, **failed**, **not executed** (section 17). Raw log and evidence of the real run in `lab/f4ui/` (outside the repository).

## What was implemented (`apps/desktop` only)

| Area | Change |
| --- | --- |
| `src/agents.rs` (new) | **Agents** page: agent access switch (*Allow agents to request sessions*, off until read, explains what turning it on allows and that LightVM does not limit what the agent's own app can do); **Pair a client…**; pending requests (client name and purpose as plain labels, what the agent asks for, countdown, **verification code** at 30 pt with the accessible name *Verification code 482-913*, *Approve only if the agent shows exactly the same code*); approval form without text fields: exactly one VM (radio; preselected only if the agent asked for one known VM), capabilities with plain explanations (stop and reset marked *Can lose unsaved work* and never preselected, even if requested), *Give keyboard and mouse to the agent now* (only with input), duration 15 min / 1 h / 4 h / 8 h, limits Small / Standard (= `AGENT_DEFAULT_QUOTAS`) / Large, lease 10 / 5 / 3 s, mandatory *The agent shows the code …* tick before **Approve session**; refusals in plain words (`resource_limit` = another session, `not_found`, `state_conflict`); **Deny**; active sessions (state, *Keyboard and mouse: agent/you*, VM, time left, usage vs quotas) with Pause/Resume, Take/Give control, Revoke…, Details; ended sessions with the cause in words; **session page** (objective, state and controls, VM, allowed actions, time window, usage, *Activity*: session events seen by the app); **indicator** (`banner_ui`) |
| `src/lib.rs` | Section **Agents** in the side bar; indicator as a top panel on **every page, full-screen console included**: *Agent session active/paused on VM · client · keyboard and mouse: agent/you · time left* with Details, Revoke…, Take/Give control, Pause/Resume and **Emergency Stop** (always enabled); a waiting request shows a blue bar with the code and **Review** on the other pages; revoke confirmation dialog (*Revoke session*, the VM keeps running, actions already sent may complete); pairing dialog (explanation → *Write pairing file* → path and expiry, the credential never shown); while any dialog is open and a session is live, the Emergency Stop is drawn again **above the dialog** at the same place (above the modal layer, so it stays clickable); console: while an agent owns the input, notice *Input controlled by the agent "…": your keyboard and mouse are not sent to the guest* with **Take control**; repaint once a second only while sessions or requests exist |
| `src/console.rs` | `set_input_blocked`: on the handover the keys/buttons held by the user are forgotten and the queued input dropped (the supervisor releases them in the guest); no keyboard focus, no pointer or key queued while blocked |
| `src/service.rs` | `AgentState` in `ServiceState` (access, requests, sessions, input owners, last approval, last control, pairing file, read error, event log); 11 commands (`GetAgentAccess`, `SetAgentAccess`, `ListSessionRequests`, `ListSessions`, `ApproveSession`, `DenySession`, `PauseSession`, `ResumeSession`, `RevokeSession`, `SetInputOwner`, `CreatePairing`); pause, resume, revoke, input owner and access go on the **fast lane** (never behind a start, a stop or a long request); events `agent_access`, `session_request`, `session_request_closed`, `session_changed`, `input_owner`, `agent_sessions_stopped`, `events_lost` applied and followed by re-reads; session log recorded once per change whatever the order of reply and event, request events attached to the session of the same agent connection; pairing file removed on access off (reply and event), Emergency Stop, service stop/drop and at the start of a new service |
| `src/pairing.rs` (new) | Pairing file `%LOCALAPPDATA%\LightVM\pairing\pairing.json` (`{format, endpoint, credential, expires_at}`); the folder gets inheritance removed and **one** ACE for the current user's SID (`icacls.exe`/`whoami.exe` from `System32`, no console window, entry count checked, write refused otherwise), temporary file + rename; the credential buffer is overwritten after writing; Unix: `0700` folder, `0600` file |
| Tests | `tests/agents_ui.rs` (9 kittests), `tests/real_agents.rs` (ignored, real QEMU + scripted agent), `tests/common/mod.rs` (requests, sessions, fixed clock), 1 IPC test in `service.rs` on the real framing, 2 in `pairing.rs`, 3 in `agents.rs`, 9 new snapshots; 23 existing snapshots regenerated (side bar now has **Agents**) |
| Docs | `docs/USER_GUIDE.md` section 11 "Agent sessions" (later sections renumbered, data table and limitation updated) |
| Other | `ServiceConfig.pairing_dir` (tests use a temporary folder); dev-dependency `sha2 =0.11.0` (same version as the workspace) for the real test, hence one line in `Cargo.lock` |

### Pairing credential: decision

`create_pairing_credential` returns the credential to the UI. ADR-0006 forbids it in process arguments and logs and asks for "OS credential store"; the app has no `unsafe` code (workspace lint), so the Windows Credential Manager API is not available to it without a new dependency or `unsafe`. The safest approach consistent with the "no clipboard" rule: **a file the adapter reads**, in a folder restricted to the current user's SID with a verified single-entry ACL (the same procedure as the supervisor's private run folders), valid 10 minutes, removed when no longer useful, never shown on screen. Same-user processes can read it, as they can already reach the IPC endpoint (threat model 7.1). **For the F5 adapter**: read `pairing.json` from `%LOCALAPPDATA%\LightVM\pairing\` (`format` = `lightvm-pairing-v1`), connect to `endpoint` with `credential`, then `request_session`.

## Results

| Item | Outcome | Evidence |
| --- | --- | --- |
| Agent access off by default, explained; switch sends `set_agent_access`; disabled while unknown; pairing disabled while off; no text field; no indicator without sessions/requests | verified (stand-in) | `agents_ui::agent_access_is_off_by_default_and_explains_what_it_allows`; real run: *agent access read: off* |
| Pairing: dialog, `create_pairing_credential`, file path and expiry shown, credential never in the UI state | verified (stand-in, fake IPC, real) | `pairing_writes_the_file_and_never_shows_the_credential`, `service::tests::agent_sessions_over_ipc`, `pairing::tests::*`; real run: ACL entries = 1, credential absent from the state, file removed at stop |
| Request card with client, purpose, requested VM/capabilities, countdown, large code; Dashboard bar with code and Review | verified (stand-in, real) | `request_is_approved_with_the_code_one_vm_and_the_chosen_limits`, snapshots `agents_request`, `agents_request_light`, `dashboard_session_request`; real run: the code received by the agent is the one shown |
| Approval: one VM, capabilities (dangerous ones marked, not preselected), input now, duration, limits, lease, code confirmation required; exact `SessionApproval` sent; refusal explained; form closes when approved | verified (stand-in, real) | same test, snapshot `agents_approval_form`; real run: approval → agent granted in 38.8 ms |
| Deny from the card and from the form | verified (stand-in) | `request_can_be_denied_from_the_card_and_from_the_form` |
| Pause/Resume from the indicator, Revoke with confirmation (Cancel sends nothing), indicator gone, ended session with cause | verified (stand-in, real) | `session_can_be_paused_resumed_and_revoked_with_confirmation`, snapshot `dialog_revoke`; real run: paused → agent input `permission_denied: session paused`, screenshot still allowed; revoke → agent connection closed, VM still running |
| Indicator on Dashboard, Console, Agents, Settings and the full-screen console; Emergency Stop clickable on each and with the supervisor disconnected; Emergency Stop clickable above an open dialog (pointer click) | verified (stand-in) | `indicator_is_on_every_page_with_the_emergency_stop`, `session_can_be_paused…` (dialog part), snapshots `dashboard_agent_session`, `console_agent_input` |
| Console: user input sent normally; after the handover nothing typed or pointed is sent, notice + Take control; Take control sends `set_input_owner user`, the owner event restores the input | verified (stand-in, real) | `console_input_is_not_sent_while_the_agent_has_control`; real run: console blocked, agent input accepted while owner, refused (`input_not_owned`) after Take control |
| Session page: objective, limits, usage, events of that session only; no text field | verified (stand-in, real) | `session_page_shows_objective_limits_and_events`, snapshot `agent_session_page`; real run: page lists request, start, handover, takeback, pause, resume, end |
| IPC: commands, replies, events, input owner recorded once though seen twice, request events attached to the session, pairing file content and removal on access off | verified (real framing, fake server) | `service::tests::agent_sessions_over_ipc` |
| `cargo clippy -p lightvm-desktop --all-targets -D warnings` (default, `--features glow`, `--features wgpu`), `cargo fmt --check`, all `lightvm-desktop` tests | verified | lib 62, agents_ui 9, baselines_ui 11, clipboard_ui 5, console_ui 11, dialogs_ui 10, kittest_ui 9, service_child 3 (+2 ignored), service_ui 6, start_progress_ui 1, visual_snapshots 29, vm_settings_ui 6, wizard_ui 4; real tests ignored by default |

## Real run (`tests/real_agents.rs`, run 1)

Setup: supervisor built from `HEAD` (`99e62f2`, the committed F4 contract; the working tree of `crates/` was being changed by the F5 agent), debug build, QEMU 11.1.0 portable, WHPX, VM 1 vCPU / 1 GiB / 4 GiB disk booting `lab/media/p5-dati.iso` (only to have a running VM with a display; no guest installed), UI headless with `egui_kittest`, agent = test thread with `lightvm_service::client`.

| Step | Observed |
| --- | --- |
| Start | agent access read **off**; VM running in 0.6 s |
| Access on + pairing (UI clicks) | file written, folder ACL **1 entry**, agent read endpoint + credential from it |
| Request | UI showed *Verification code 254-389* = code received by the agent |
| Approval (UI: VM preselected, input now, code ticked) | agent `session_granted` 38.8 ms after the click, capabilities `[vm.list, vm.status, vm.screenshot, vm.input]`; indicator shown, input with the agent |
| Agent screenshot | 7503-byte PNG in 1 chunk, SHA-256 equal to the metadata (`lab/f4ui/evidence-1/agent-screenshot-1.png`: the firmware had not drawn yet, *Display output is not active*) |
| Agent input as owner | accepted |
| Console (UI) | *Input controlled by the agent …*, user input blocked |
| Take control (indicator) | agent input refused: `permission_denied … reason=input_not_owned` |
| Pause (indicator) | agent input refused: `permission_denied: session paused`; agent screenshot still served (5817 bytes, SHA-256 ok) |
| Resume, Revoke… → Revoke session | session terminated in the UI in 102.9 ms; the agent's next call failed with the pipe closing; **VM still running**; indicator gone; ended session *revoked by you*; session page events as listed above |
| Stop | supervisor stopped cleanly (exit 0), pairing file removed |
| Audit (`lab/f4ui/evidence-1/audit.jsonl`) | `agent_access_enable` 1, `session_request` 2, `session_create` 1, `input_control` 2, `screenshot_read` 2, `session_pause` 1, `session_resume` 1, `session_terminate` 1 |

Log: `lab/f4ui/run1.log`. One run, 17.9 s; values from one host, not guarantees.

## Deviations and limits

- **Pairing by file**, not the OS credential store (see the decision above); to be recorded in ADR-0006/SECURITY.md by the owner of those files. If the UI dies while the supervisor keeps running (`keep_vms`), the file stays until the credential expires (10 min) or the next supervisor start.
- **Activity on the session page** = session events seen by the app (request, start, pause/resume, input handover/takeback, end). The per-call audit is not exposed over IPC at `HEAD`; the F5 agent is adding session reports (`list_session_reports`, `get_session_report`, report events and screenshots) in the working tree, not committed: the page should show them once they land. **No artefacts section**: no real artefact data exists yet (no simulated data).
- The Emergency Stop above dialogs is drawn only while a session is live; otherwise dialogs block the window as before (the Dashboard's Emergency Stop is reachable once the dialog closes).
- The approval form offers only the capabilities served in F4 (list, status, screenshot, input, start, stop, reset); `vm.snapshot`, `vm.restore`, `vm.logs` arrive with F5 and are shown (explained) on the session page if granted.
- kittest pointer clicks miss widgets outside the visible area and can land on the tooltip of the previous click: the agents kittests click through AccessKit (`click_accesskit`, as assistive technology does); the Emergency Stop above a dialog is tested with a real pointer click.
- One live session at a time (service MVP limit): the UI lists several but the indicator is designed for one or two.

## Not executed

- A real MCP client (Claude Desktop or other): the adapter is F5.
- A human user at the physical screen (keyboard, focus, screen reader), high-DPI.
- Linux (pairing file with `0700`/`0600` permissions is untested on Linux).
- Agent input on an installed Ubuntu through a UI-approved session (the backend verified it in `F4-01` with the IPC directly).

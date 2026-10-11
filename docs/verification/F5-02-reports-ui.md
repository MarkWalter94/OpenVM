# F5-02 — Session reports in the desktop: AI Workspace, history, report page, verdict, export, deletion

- Execution date (UTC): 2026-10-11
- Executed by: development agent "UI F5" (Claude Code session); F5 work split (backend and MCP adapter in `F5-01`, edited concurrently in `crates/`)
- Roadmap step: section 10 F5 — "Schermata sessione con obiettivo, azioni, limiti e artefatti", "AI Workspace e cronologia Sessions", "Report JSON/Markdown e stati di esito distinti" (UI side), export controlled by the user; section 9 (screenshots may contain secrets: explicit capture/retention controls and deletion)
- Related ADRs: ADR-0019 (fixture verifier, run by the service), ADR-0022 (UI tests); none modified
- Contract used: F5 contract §4–5 (committed in `16593ec`): `list_session_reports`, `get_session_report`, `get_session_report_events`, `get_report_screenshot_chunk`, `set_session_verdict`, `verify_session`, `export_session_report`, `delete_session_report`, event `session_report_changed`; types `lightvm_proto::report::*`. No change to `crates/`.
- Overall outcome: **partial** — every flow is **verified** headless with the stand-in supervisor (kittests, snapshots) and on the real IPC framing (fake server); **not executed** against the real supervisor with a real agent session (no real run in this step).

Allowed outcomes: **verified**, **failed**, **not executed** (section 17).

## What was implemented (`apps/desktop` only)

| Area | Change |
| --- | --- |
| `src/service.rs` | `ReportsState` in `ServiceState` (list, headers, events page per report, last events of live sessions, screenshots read on demand — at most 3 in memory —, last action and its outcome); 8 commands (`ListSessionReports`, `GetSessionReport`, `GetSessionReportEvents {tail}`, `GetReportScreenshot`, `SetSessionVerdict`, `VerifySession`, `ExportSessionReport`, `DeleteSessionReport`); screenshot read chunk by chunk (same shot, index, chunk count and SHA-256 on every chunk, ≤ 64 MiB), **SHA-256 checked in the app** and PNG decoded off the IPC task, never written to disk; export on the slow lane (it can copy hundreds of MiB of screenshots); `session_report_changed` → list re-read plus header and events page of a report the app shows (`report_changed`); list re-read at connection and after `events_lost`; `not_found` on a header forgets the report |
| `src/reports.rs` (new) | Outcome texts and colours (six distinct texts), source texts (*pending: session running*, *from how the session ended*, *by the automatic verifier*, *your verdict*), session history, report sections of the session page (outcome + source + verdict history; **Agent's declaration** under a notice *"What the agent claims about its own work. It is not the outcome …"*; limits and policy revision; versions, VM configuration hash, baseline, runs, errors by code, report SHA-256; events by page of 50 with *Show* on stored screenshots; screenshots with counts, retention text, image shown only on request with full SHA-256 "(checked)"; export and deletion), event texts (plain text, agent texts never interpreted) |
| `src/agents.rs` | Page title **AI Workspace**: *Current session* (client, VM, time left, objective, usage "So far", last 5 recorded actions, *Show the agent's latest screenshot* on request, controls), *Session requests*, *Agent access*, *Session history* (reports; ended sessions without a report still listed); session page fed by the report (also for sessions only known from reports, e.g. after a supervisor restart), with the app's own events under *Seen by this app* |
| `src/lib.rs` | Report dialog (verdict, automatic check, export, deletion) with `dialogs::confirm_custom`; `ReportPicker` (native `rfd` save dialog with JSON/Markdown filter, fake in tests) and `report_file_name` (`lightvm-session-<client>-<yyyymmdd>-<hhmm>.<ext>`); re-read every 3 s of the live sessions' report header and last events (the service has no event per agent action) and of the events page of the live session shown; textures of the screenshots shown, dropped when hidden; opening a report reads its header and first page |
| `src/dialogs.rs` | `confirm_custom` (choices between explanation and buttons); `confirm_with_consent` uses it |
| `src/main.rs` | `set_report_picker(native_report_picker())` |
| `Cargo.toml` | `sha2 =0.11.0` moved from dev-dependencies to dependencies; `png =0.18.1` added (both already in `Cargo.lock`, same versions; one line added to the desktop entry of the lock) |
| Tests | `tests/reports_ui.rs` (11 kittests), 1 IPC test and 2 unit tests in the lib, fixtures in `tests/common/mod.rs` (`report`, `report_events`, `shot_image`), 5 new snapshots, 6 regenerated; `agents_ui.rs` and `real_agents.rs` adapted (history label, report button) |
| Docs | `docs/USER_GUIDE.md` section 11, points 6–9 (AI Workspace, reports and outcomes, verdict, automatic check, export, retention and deletion) |

### Decisions

- **Verdict rationale.** `set_session_verdict` requires a non-blank rationale; the contract has no reason enum and the UI has no text field. The app sends a fixed text (`USER_VERDICT_RATIONALE`: *"Judged by the local user in the LightVM app after reviewing the session report."*); the service attributes the verdict to `local user`. The user only chooses passed / failed / inconclusive, then confirms.
- **Automatic check** (`verify_session`, fixture marker present/absent) is offered with a confirmation, disabled when the report has no stored screenshot.
- **Export overwrite.** The first request is always `overwrite: false`; on `state_conflict` the dialog asks *Replace the file* and only then sends `overwrite: true` (the Windows save dialog may also have asked: the user confirms twice in that case). `include_screenshots` is off by default, with a warning when ticked.
- **Screenshots on demand.** No screenshot is read unless the user presses *Show* (report page) or *Show the agent's latest screenshot* (workspace); once the workspace view is on, it follows the latest screenshot of the live session (re-read with the header) until *Hide* or the end of the session. Nothing is written to disk by the app.
- **Live updates by polling.** `session_report_changed` arrives at approval, termination, verdict, verification and deletion only, so the live session's header and last events are re-read every 3 s while the Agents page is visible.

## Results

| Item | Outcome | Evidence |
| --- | --- | --- |
| History: newest first, VM, start/end UTC, cause, counts, outcome badge with text for all six outcomes and source (verifier / user / termination / pending), live sessions included; no text field | verified (stand-in) | `reports_ui::history_lists_every_outcome_with_its_source`, snapshot `agents_workspace` (lower part) |
| Session page from the report for each outcome with its source text; header and first events page requested on opening; limits, policy revision, versions, baseline, errors; delete disabled for a live session | verified (stand-in) | `session_page_shows_each_outcome_and_where_it_comes_from`, snapshots `report_page`, `agent_session_page` |
| Agent declaration labelled as the agent's claim, plain text (markup shown literally), outcome unchanged | verified (stand-in) | `agent_declaration_is_labelled_as_a_claim_not_the_outcome` |
| Events paged by 50 (Previous/Next send the right offset); screenshot read only after *Show*, not read again when already read, *Hide*; integrity failure shown, image not shown | verified (stand-in) | `events_are_paged_and_screenshots_are_read_only_on_request` |
| Verdict: dialog with three choices, none preselected, confirm disabled until chosen, Cancel sends nothing, exact `SetSessionVerdict`, waiting state, dialog closes on success, outcome *your verdict* and history shown | verified (stand-in) | `verdict_is_explicit_attributed_and_confirmed`, snapshot `dialog_report_verdict` |
| Automatic check: confirmation, exact `VerifySession`, failure shown in the dialog and on the page; disabled without stored screenshots | verified (stand-in) | `automatic_check_is_confirmed_and_needs_a_screenshot` |
| Export: format and screenshots choice (warning), fake save dialog gets the proposed name and format; cancelled picker sends nothing; `overwrite:false`, then *Replace the file* → `overwrite:true`; where it was written (path, size, SHA-256, screenshot folder) in the dialog and on the page; no picker → explained | verified (stand-in) | `export_goes_through_the_save_dialog_and_says_where_it_was_written`, `export_without_a_save_dialog_explains_why`, snapshot `dialog_report_export` |
| Deletion: confirmation (what is removed, exported copies untouched, cannot be undone), Cancel sends nothing, `DeleteSessionReport`, back to the workspace with *Report deleted* | verified (stand-in) | `report_is_deleted_only_after_confirmation`, snapshot `dialog_report_delete` |
| Workspace: live session at the top with objective, usage, last recorded actions; live report re-read (header + tail); screenshot only on request (latest index from the header), then shown, *Hide* | verified (stand-in) | `workspace_shows_the_live_session_and_its_screen_only_on_request`, snapshot `agents_workspace` |
| `session_report_changed` re-reads the list and, for a report shown, its header and events page | verified (unit, real framing) | `report_changed_event_rereads_what_is_shown`, `service::tests::session_reports_over_ipc` |
| IPC: list, header, events, screenshot in 2 chunks reassembled, SHA-256 checked and PNG decoded; altered screenshot refused; verdict updates header and list; export conflict then overwrite with the user's path; deletion forgets the report | verified (real framing, fake server) | `service::tests::session_reports_over_ipc` |
| `cargo clippy -p lightvm-desktop --all-targets -D warnings` (default, `--no-default-features --features glow`, `--features wgpu`), `cargo fmt --check` | verified | — |
| All `lightvm-desktop` tests | verified | lib 65, agents_ui 9, baselines_ui 11, clipboard_ui 5, console_ui 11, dialogs_ui 10, kittest_ui 9, reports_ui 11, service_child 3 (+2 ignored), service_ui 6, start_progress_ui 1, visual_snapshots 32, vm_settings_ui 6, wizard_ui 4; real tests ignored by default |

Snapshots: new `agents_workspace`, `report_page`, `dialog_report_verdict`, `dialog_report_export`, `dialog_report_delete`; regenerated `agent_session_page`, `agents_sessions`, `agents_request`, `agents_request_light`, `agents_approval_form`, `dialog_revoke` (page title and order changed). Images checked by eye; the screenshot in the snapshots is the deterministic test pattern of `tests/common`, not a guest screen.

## Deviations and gaps

- **No "Delete screenshots" alone**: the contract only offers `delete_session_report` (report and screenshots together). The UI says so and points to export-then-delete. A screenshot-only deletion needs a new request in `crates/` (owner of the contract).
- **No real run** with the supervisor of this commit and a real agent: `tests/real_agents.rs` was only adapted (it opens the ended session through its report) and compiled, not executed.
- **Live updates are polled** (3 s) because the service publishes no event per agent action; an event such as `session_report_appended` would remove the polling.
- **Verdict rationale is a fixed text** (no reason enum in the contract, no text field in the UI).
- The workspace thumbnail asks for the last screenshot index; if that one was not stored (retention limit) it says so rather than looking for an earlier stored one.
- History is not paged: the contract returns at most 100 reports in one list.
- The Windows save dialog (`rfd`) is not exercised by tests (fake picker); not executed on Linux (no picker there: export explains it is not available).

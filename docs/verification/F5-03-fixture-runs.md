# F5-03 — Ten fixture runs end to end and the six outcome states on real QEMU (scripted MCP client)

**Follow-up, 2026-10-18:** [F5-04](F5-04-real-clients.md) separately closes
the two-real-LLM-client requirement. These ten infrastructure attempts
remain scripted; they are not relabelled as model-driven runs.

- Execution date (UTC): 2026-10-11, 05:43–06:24 (one test run, 2 325 s)
- Executed by: development agent "F5/F6 backend" (Claude Code session)
- Roadmap step: section 10 F5, exit criterion "fixture deterministica completata e ripristinata dieci volte senza errori infrastrutturali; ... report corretti anche per fallimento, timeout e inconcludente. Registrare tutti i tentativi del campione, inclusi i fallimenti." Sections 7.3, 8, 9.
- Related ADRs: ADR-0019 (fixture), ADR-0021 (MCP), ADR-0009 (Emergency Stop); none modified
- Overall outcome: **verified for the infrastructure part** — 10/10 attempts completed and restored with **0 infrastructure errors**, and one real report for each outcome state (`passed`, `failed`, `inconclusive`, `cancelled` ×2, `timed_out`, `infrastructure_error`), all on real QEMU. **This is not the F5 exit criterion**: the client is a deterministic scripted MCP client, **not an LLM**; the "at least one successful path with each of two real LLM clients" part is still **not executed** (F5-01).

Allowed outcomes: **verified**, **failed**, **not executed** (section 17). Evidence in `lab/f5/evidence/f5-03-sample/` (outside the repository, 19 MB): `sample.tsv`, `outcomes.tsv`, `harness.log`, and per attempt `client-transcript.jsonl` (every JSON-RPC message, images elided), `adapter-stderr.txt`, exported `report.json` + `report.md` + `report-screenshots/`, `after-restore-mostra.png` (V6), boot screenshots; console log `lab/f5/f5-03-sample.log`.

## What was run

Test `crates/lightvm-mcp/tests/f5_real_client.rs::f5_fixture_sample_and_outcome_states` (`#[ignore]`, new in this step, beside the F5-01 test), release build, one service and one catalog for the whole sample:

```
$env:LIGHTVM_E2E_QEMU_DIR = "lab\qemu\qemu-11.1.0"
$env:LIGHTVM_F5_EVIDENCE_DIR = "lab\f5\evidence"; $env:LIGHTVM_F5_RUN = "f5-03-sample"; $env:LIGHTVM_F5_RUNS = "10"
cargo test --release -p lightvm-mcp --test f5_real_client -- f5_fixture_sample_and_outcome_states --ignored --exact --nocapture
```

Setup (once): Ubuntu 26.04.1 live ISO and the ADR-0019 fixture ISO imported through the UI-role IPC (`import_media`, hashes verified); VM `pc-q35-11.1`, `Skylake-Client-v4`, 4 vCPUs, 4096 MiB, 8 GiB empty data disk, offline; baseline of the empty disk (forced stop in the firmware, `allow_unclean`, recorded *not verified* — D-7 of F5-01) `01a1297e-bf15-73da-be14-a3c4a0caa41b`, disk SHA-256 `e80c9453…63014d2`; restore onto an overlay; agent access turned on; first boot to the live desktop (A0: two identical screenshots 5 s apart, ≥ 99 % lit) after 121.8 s, no fixture colour on screen.

Each **attempt** (`run-NN`):

1. A new pairing credential and pairing file (private folder) as the app writes it; the real `lightvm-mcp` adapter started over stdio; the **scripted client** (not a model) speaks JSON-RPC to it: `initialize` (2025-11-25), `tools/list`, `session_request`, `session_status` until approved, `vm_list`, `vm_screenshot` (half), `vm_keyboard_press ctrl+alt+t`, `vm_screenshot` (half), `vm_keyboard_type` (the fixture command `… sh /mnt/fx.sh scrivi`), `vm_screenshot` (full), `vm_logs`, `report_get`, `session_declare(inconclusive)`, then one more `vm_list` after the revocation (must be refused).
2. **Approval through the UI-role IPC** by the harness, standing in for the user's click in "Agents" (VM, `vm.list/status/screenshot/input/logs`, 30 min, input handed over).
3. Once the declaration is in the report: **revocation** by the harness (user's Revoke).
4. **Verifier**: `verify_session` with the ADR-0019 pixel verifier, expected marker present, on the last stored full screenshot.
5. **Report exported** (JSON with screenshots, Markdown) through the UI-role IPC.
6. **Restore**: forced stop, `restore_baseline`, fresh boot to the desktop, the harness types `fx.sh mostra` → **V6: marker absent** expected; the terminal is closed and the desktop checked free of fixture colours, so the next attempt starts from a clean desktop of a restored VM (otherwise a further restore and boot would have been done: never needed).

**Infrastructure error** of an attempt = any of: a tool call refused or failed other than the final expected `session_closed`; the adapter exiting with an error or its output ending early; no session; no stored screenshot; an export or restore failing; V6 not `Absent`. A fixture outcome other than `passed` would have been recorded as such, not as an infrastructure error.

## The sample (all attempts; none excluded)

| Attempt | Session | Outcome (source) | Termination | Verifier rationale | Shots / events / calls | Infrastructure errors | V6 after restore | Restore | Duration |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| run-01 | `01a12980-ae74…` | passed (verifier) | revoked | V3 band B1..B4, B5 marker present (1280×800) | 3 / 99 / 12 | 0 | absent | 67 ms | 179 s |
| run-02 | `01a12983-6c42…` | passed (verifier) | revoked | same | 3 / 99 / 12 | 0 | absent | 104 ms | 166 s |
| run-03 | `01a12985-f58e…` | passed (verifier) | revoked | same | 3 / 99 / 12 | 0 | absent | 108 ms | 185 s |
| run-04 | `01a12988-c9c7…` | passed (verifier) | revoked | same | 3 / 99 / 12 | 0 | absent | 66 ms | 180 s |
| run-05 | `01a1298b-8a0f…` | passed (verifier) | revoked | same | 3 / 99 / 12 | 0 | absent | 64 ms | 180 s |
| run-06 | `01a1298e-49f6…` | passed (verifier) | revoked | same | 3 / 99 / 12 | 0 | absent | 66 ms | 179 s |
| run-07 | `01a12991-089b…` | passed (verifier) | revoked | same | 3 / 99 / 12 | 0 | absent | 61 ms | 180 s |
| run-08 | `01a12993-c80a…` | passed (verifier) | revoked | same | 3 / 99 / 12 | 0 | absent | 69 ms | 181 s |
| run-09 | `01a12996-8b32…` | passed (verifier) | revoked | same | 3 / 99 / 12 | 0 | absent | 69 ms | 179 s |
| run-10 | `01a12999-49ea…` | passed (verifier) | revoked | same | 3 / 99 / 12 | 0 | absent | 67 ms | 179 s |

Totals: **10 attempts, 10 completed and restored, 0 infrastructure errors**, 10 `passed` by the verifier (the client declared `inconclusive` every time: the declaration never sets the outcome), 10 V6 `absent`. Restore median 67 ms (max 108 ms); attempt median 180 s (boot to the desktop ≈ 120 s of it). Each verified screenshot hash is in `sample.tsv` and in the report. In every exported report and transcript: no host path, the typed fixture command absent from the audit (`audit: 10 539 events; typed command present in audit: false`).

This is a sample of ten, not a statistical estimate; with a scripted client it shows that the chain *adapter → service → core → QEMU → report → export → revoke → restore* repeats without infrastructure errors. A real LLM may fail the fixture for its own reasons; per ROADMAP 9 such failures are recorded, not hidden, and are not infrastructure errors.

## Reports for the other outcome states (real QEMU, same service and VM, right after the sample)

| Case | How it was produced | Expected | Report outcome (source) | Termination | Client calls (`!` = refused) | Unexpected client errors | VM after |
| --- | --- | --- | --- | --- | --- | --- | --- |
| `outcome-failed` | the client does the **wrong action on purpose** (`fx.sh mostra` instead of `scrivi`, no marker written) and declares `passed`; harness revokes, verifier expects the marker present | failed | **failed** (verifier: "V3 band found but B5 marker Absent, expected Present") | revoked | `session_request, session_status, vm_list, vm_screenshot, vm_keyboard_press, vm_keyboard_type, vm_screenshot, session_declare, vm_list!session_closed` | 0 | running |
| `outcome-inconclusive` | a screenshot, then `session_end`; **no verifier, no user verdict** | inconclusive | **inconclusive** (termination default) | completed | `…, vm_screenshot, session_end` | 0 | running |
| `outcome-cancelled-revoke` | the user's Revoke (UI-role IPC) after the first screenshot | cancelled | **cancelled** | revoked | `…, vm_screenshot, vm_status!session_closed` | 0 | running |
| `outcome-timed-out` | approval for the minimum duration (60 s); the client keeps polling `vm_status` every 2 s; nobody acts | timed_out | **timed_out** | grant_expired | 28 × `vm_status`, the last `!session_closed` (61 s) | 0 | stopped (session VM stopped on the emergency path, ROADMAP 7.3) |
| `outcome-cancelled-emergency-stop` | **Emergency Stop** through the UI-role IPC after the first screenshot (`revoked_connections: 1, processes_terminated: 1, completed: true`) | cancelled | **cancelled** | emergency_stop | `…, vm_status!session_closed` | 0 | stopped |
| `outcome-infrastructure-error` | **QEMU killed from outside the service** (`Stop-Process -Force` on the `qemu-system-x86_64` of this catalog, pid 8864) after the first screenshot — a crash stand-in | infrastructure_error | **infrastructure_error** | vm_terminated | `…, vm_status, vm_status!session_closed` | 0 | stopped (brought back from `failed` by the supervisor) |

`passed` is the ten attempts above. All six cases matched the expected outcome; every report was exported (JSON + Markdown) into its folder. Then a final forced stop and baseline restore.

Notes on the design of the states:

- The Emergency Stop is a **user action**: by the existing rule (`reports::termination_outcome`, F5-01) it yields `cancelled`, not `infrastructure_error`. The infrastructure error is produced by an **unrequested** end of QEMU (crash or external kill).
- Producing that state on real QEMU found two defects, fixed in this step (details and tests in F6-01 §4): (1) a crash or external kill of QEMU left the agent session live on a dead run — now the core ends it with `vm_terminated` (report `infrastructure_error`); (2) a VM left `failed` stayed `failed` forever (no start/restore/delete even after a restart) — now the supervisor brings it back to `stopped` through `recovering` at the next observation, and a restart reconciles a persisted `failed` VM. The table's last row shows both on real QEMU (session closed with `vm_terminated`, VM `stopped` 3 s later, baseline restore then worked).
- `timed_out` was produced by the grant expiry (60 s minimum duration). The lease expiry path (adapter alive but not renewing) is covered by `f4::lease_expiry_terminates_the_session_and_stops_the_vm` and `f5_reports` with the fake backend (F5-01): the real adapter always renews.

## Environment

| Item | Value |
| --- | --- |
| Host | Intel Core i9-14900K, Windows 11 Pro 10.0.26300, WHPX; host shared with another development agent during the run |
| Build | `--release`, `CARGO_TARGET_DIR=target\f6run` (deleted afterwards), working tree of this step at 05:43 UTC — it already contained the two crash fixes above; the later F6 changes (start free-space check, re-probe before start, report storage reserve, host-resume detection) were made after the build and are covered by their own tests (F6-01), not by this sample |
| QEMU | 11.1.0 portable `lab/qemu/qemu-11.1.0` (approved hashes) |
| Guest | `lab/media/ubuntu-26.04.1-desktop-amd64.iso` SHA-256 `601e30fb…1bda1f`, live session; fixture ISO from `lightvm-fixture` |

## What is missing for the F5 exit criterion

1. **Real LLM clients**: still 0 executed (F5-01: Claude Code logged out; a second client with native MCP stdio not chosen). The same harness runs a real client with `LIGHTVM_F5_CLIENT=claude` (F5-01 test).
2. The sessions UI driven by a person (approval click, verdict, export): the harness used the UI-role IPC as a stand-in.
3. Linux: not executed.

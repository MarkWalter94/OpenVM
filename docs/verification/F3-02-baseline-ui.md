# F3-02 — Baselines page of the desktop UI (creation, restore, deletion) and start progress

- Execution date (UTC): 2026-10-11
- Executed by: development agent "UI F3" (Claude Code session), F3 work split (backend in `F3-01`)
- Roadmap step: section 10 F3, "schermata baseline con creazione, ripristino ed eliminazione autorizzata"; section 8 (model: "baseline a VM spenta", no RAM state, forced stop never promoted to verified, deletion only if not referenced, restore with normal boot)
- Related ADRs: ADR-0005, ADR-0022 (UI tests), ADR-0025
- Overall outcome: **partial** — the page and its flows are **verified** headless with the stand-in supervisor (kittests, snapshots) and **verified** end to end with the real supervisor, real QEMU 11.1.0 and real `qemu-img` driven through the UI (run 5 below, see its outcome); **not executed**: a baseline of an installed Ubuntu through the UI, a long (multi-minute) creation cancelled by the Emergency Stop from the page with real QEMU, a human user at the physical screen.

Allowed outcomes: **verified**, **failed**, **not executed** (section 17). Raw logs and rendered screens of the real runs are in `lab/f3/ui/` (outside the repository).

## What was implemented (`apps/desktop` only)

| Area | Change |
| --- | --- |
| `src/baselines.rs` (new) | Baselines page of a VM: summary of the current state (*Standalone disk* or *Running on baseline "…"* with the bytes written since the restore, VM state or running operation, number and size of baselines); list, most recent first, with label, creation time (UTC), size on the host, disk size, "taken on" parent, badges **Current**, **Verified** / **Not verified**, **Protected**, **In use**; **Create baseline…**, **Restore…**, **Delete…** with the reason in the tooltip when disabled; progress card of a long operation (spinner, elapsed time from 5 s, "no time limit, only the Emergency Stop cancels"); outcome notice; plain-language texts for every refusal (`error_text`) |
| Labels | Generated, no text field: `Baseline N · YYYY-MM-DD HH:MM UTC` (first free N). The UI keeps a single text field (the VM name in the wizard). Renaming is not offered |
| `src/lib.rs` | Page opened from the Dashboard row (**More** → **Baselines…**), Back / sidebar Dashboard closes it; baseline confirmation dialog: creation, consent step after an `unclean_shutdown` refusal (checkbox *I understand: the baseline is marked Not verified* enables **Create unverified baseline**, sent with `allow_unclean: true`), restore (**Discard and restore**, explains what is lost, normal boot, "create a baseline first to keep the current state"), deletion (**Delete baseline**, waits for the outcome); the dialog of a creation/restore closes as soon as the VM shows the operation running (or after 2 s) so the Emergency Stop stays reachable; stale dialogs refused (VM no longer stopped, operation already running, baseline gone or now in use); VM delete / media removal / settings save refused while a baseline operation runs; polling of the VM list every 500 ms while a VM is starting |
| `src/dashboard.rs` | Row line *On baseline "…" · N of changes since the restore · K baselines (size)*; pill *Creating baseline* / *Restoring baseline*; Start, Delete, Remove media disabled while busy; **Baselines…** item (disabled without `baseline_available`); start progress line: *Verifying installation media… 42 %* / *Verifying baseline… 75 %* / *Launching…* with an accessible progress bar (same text as its name) |
| `src/dialogs.rs` | `confirm_with_consent`: optional consent checkbox that enables the action button |
| `src/service.rs` | Commands `ListBaselines`, `CreateBaseline`, `RestoreBaseline`, `DeleteBaseline`; state `baselines` (per VM, last list read) and `baseline` (last operation and outcome); `Failure` now keeps the reply's `code` and `details` (`reason`, `needed_bytes`, `free_bytes`, `budget_gib`); event `baselines_changed` → re-read of VM list and baselines; after a VM list read, baselines of VMs whose count changed are re-read; two new lanes (connections): **storage lane** for create/restore (reply when done, no timeout) and **start lane** for `Start`/`OpenVm` (reply after verification and launch); the request lane stays free for list, status, deletion; each lane matches replies by `request_id` on its own connection; pending baseline operations fail when the supervisor goes away |
| Tests | `tests/baselines_ui.rs` (11 kittests), `tests/start_progress_ui.rs` (1), 3 unit tests in `service.rs` on the real framing (fake IPC server extended with events and held replies), 2 in `baselines.rs`, 7 new snapshots, `tests/real_baselines.rs` (ignored, real QEMU) |
| Docs | `docs/USER_GUIDE.md` section 10 "Baselines" (later sections renumbered), limitation updated |

## Results

| Item | Outcome | Evidence |
| --- | --- | --- |
| Page lists baselines with label, created, verified/not verified, protected, size, current, in use; VM summary *Running on baseline X* + overlay size; Dashboard line | verified (stand-in) | `baselines_ui::page_lists_the_baselines_and_the_current_state`, snapshots `baselines`, `baselines_light`, `dashboard_baselines`, `baselines_empty` |
| No text field on the page and in the dialogs | verified | `common::text_fields == 0` in the page and creation tests |
| Creation: confirmation, command with the generated label, wait state, dialog closes when the operation runs, progress on the page, every change of the VM disabled, Start disabled on the Dashboard, Emergency Stop reachable and sent, outcome | verified (stand-in) | `creation_is_confirmed_and_shown_as_a_long_operation`, snapshot `baselines_creating`, `dialog_baseline_create` |
| Unclean stop: refusal turned into the consent step; action disabled until consent; `allow_unclean: true` sent | verified (stand-in), verified (real, run 5) | `unclean_stop_requires_explicit_consent`, snapshot `dialog_baseline_unclean` |
| Restore: confirmation explains the loss (since the restore of X / the whole disk), normal boot; integrity refusal explained in the dialog and on the page | verified (stand-in) | `restore_explains_the_loss_and_reports_integrity_errors`, `restore_from_the_standalone_disk_warns_about_the_whole_disk`, snapshot `dialog_baseline_restore` |
| Deletion: confirmation, wait for the outcome, `baseline_referenced` / `baseline_protected` explained, done closes; current/protected/in-use disabled | verified (stand-in) | `deletion_waits_for_the_outcome_and_explains_refusals`, `protected_baselines_cannot_be_deleted` |
| Refusals in plain language: `vm_busy`, `resource_limit` (free space with reserve, budget), Emergency Stop cancellation, `unsupported`, "no disk yet" | verified (stand-in) | `supervisor_refusals_are_explained`, unit `baselines::errors_are_explained` |
| Running VM / no image tool / another operation pending disable the changes | verified | `running_vm_and_missing_capability_disable_the_changes`, `one_baseline_operation_at_a_time` |
| A long creation does not block the VM list, the baselines list or a deletion; reply code and `reason` kept; `baselines_changed` re-reads the list; pending operation fails if the supervisor goes away | verified (real framing, fake server) | `service::tests::baseline_creation_does_not_block_other_requests`, `baselines_changed_rereads_the_list`, `pending_baseline_operation_fails_when_the_supervisor_is_gone` |
| A start (slow reply) does not block other requests | verified (fake server) | `service::tests::start_does_not_block_other_requests` |
| Start progress text + accessible progress bar, polling every 500 ms only while a VM is starting | verified (stand-in) | `start_progress_ui::start_progress_is_shown_and_polled` |
| End to end with real supervisor + QEMU + `qemu-img`, through the UI | see "Real runs" | `tests/real_baselines.rs`, `lab/f3/ui/run-*.log`, PNGs |

Test totals (`cargo test -p lightvm-desktop`, debug, Windows, `CARGO_TARGET_DIR=target\f3ui`): all suites green — lib 56 unit tests, `baselines_ui` 11, `start_progress_ui` 1, `visual_snapshots` 20 (7 new: `baselines`, `baselines_light`, `baselines_empty`, `baselines_creating`, `dashboard_baselines`, `dialog_baseline_create`, `dialog_baseline_unclean`, `dialog_baseline_restore`; `dialog_delete` regenerated: only the mouse pointer moved because the **More** menu has one more item), the other suites unchanged; ignored: the real-QEMU tests. `cargo clippy -p lightvm-desktop --all-targets -- -D warnings` clean with default features, `--no-default-features --features glow` and `--no-default-features --features wgpu`; `cargo fmt -p lightvm-desktop` applied.

## Real runs (`tests/real_baselines.rs`)

Setup: real `lightvm-service` built from the working tree (which at that moment also contained the concurrent backend agent's uncommitted changes, among them `VmSummary.start`), QEMU 11.1.0 portable `lab/qemu/qemu-11.1.0`, catalog in `%TEMP%\lightvm-e2e-f3-<pid>` (removed afterwards), WHPX. The wizard's installation ISO is the small `lab/media/p5-dati.iso` (8 MB): the free disk was 24–49 GB during the session and the 6.5 GB Ubuntu ISO would have been copied into the catalog; the baseline path does not need a guest system, the VM boots into the UEFI firmware and the copied disk is the VM's empty QCOW2 plus its UEFI variables. Everything is driven through the AccessKit tree of the real app (clicks on the named buttons, checkbox), the UI talks to the real service over IPC.

| Run | Outcome | Notes |
| --- | --- | --- |
| 1 | failed (UI defect found and fixed) | Orderly **Stop**: the firmware ignores the ACPI power button, the service quit QEMU after its 60 s timeout (`graceful stop: phase Quit, cause QmpQuit`). **Create baseline…** was disabled: the UI treated `VmSummary.run_id == None` as "never started", but the service does not report the last run once the VM is stopped. Fixed: the button no longer depends on `run_id`; a VM that never ran is refused by the service and explained ("start it once"). |
| 2, 3 | failed (test harness / host) | Wizard navigation of the test too eager (fixed), then the wizard could not create the VM: free disk 24 GB minus the 20 GiB host reserve left less than the minimum disk. Stopped to keep disk usage low. |
| 4 | failed (test harness race, fixed) | With 49 GB free: VM created, first run, stop (service-forced after 60 s) → **Create baseline** refused with `unclean_shutdown` → consent dialog → **Create unverified baseline** → baseline *Not verified* (0.2 s for the empty disk) → **Discard and restore** (0.1 s) → *Running on baseline "Baseline 1 · …"*, its **Delete…** disabled → Dashboard line *On baseline …* → start on the overlay (0.4 s to running, baseline verified by the service before the launch) → stop. The second creation was refused (`unclean_shutdown`) and the test read the outcome one frame before the dialog switched to the consent step. |
| 5 | **verified** | Same flow with the race fixed, plus a second baseline taken on the overlay, its restore, deletion of the first, VM deletion (log `lab/f3/ui/run-5.log`, screens `0-empty.png` … `7-final.png`). |

Run 5 (169 s in total, of which 2 × 60 s are the service's stop timeout):

```text
E2E VM running (first run): 0.4 s
lightvm-service: graceful stop: phase Quit, cause QmpQuit, 60.1439807s
E2E creation outcome or consent: 0.0 s            (refused: unclean_shutdown -> consent step)
E2E baseline created (not verified): 0.2 s
E2E baselines after create: [("Baseline 1 · 2026-10-11 03:44 UTC", false, false)]
E2E first baseline restored: 0.1 s                (page: Running on baseline "Baseline 1 · …", its Delete disabled)
E2E VM running on the overlay: 0.4 s              (Dashboard: On baseline "Baseline 1 · …")
E2E baseline created (not verified): 0.2 s        (second one, taken on the overlay: "taken on" the first)
E2E second baseline restored: 0.1 s
E2E first baseline deleted: 0.1 s
E2E baselines at the end: [("Baseline 2 · 2026-10-11 03:45 UTC", false, true)]
lightvm-service: VM deleted: 10 files (1 MiB), 7 directories
E2E service_stop=Some((Some(0), false))
```

Audit of the supervisor for the run (no paths, no labels): `baseline_create` failed `state_conflict` (the refusal) → `accepted` → `succeeded`, `baseline_restore` `accepted` → `succeeded`, the same for the second pair, `baseline_delete` `succeeded`. The catalog was removed at the end.

What run 5 does not show: the disk is an empty QCOW2 (sub-second operations, so the progress card is only seen in the stand-in snapshot); no guest file system; the second creation is also "not verified" because the stop was again forced by the service timeout.

Observations from the real runs:

- The orderly stop of a guest that ignores ACPI ends after the service's 60 s timeout and counts as an unclean stop: the consent step is exactly what the user sees in that situation (live session, firmware without OS), as intended by ROADMAP 8.
- The baseline labels use the system clock in UTC (`Baseline 1 · 2026-10-11 03:41 UTC` in run 4).

## Deviations and limits

- **Labels generated** (`Baseline N · date UTC`), no text field and no renaming: chosen to keep the single-text-field rule of the UI. A label field could be added later in the creation dialog (one labelled field) if users need names.
- **One baseline operation at a time across all VMs** in the UI (the service allows one per VM): the UI keeps one "last baseline operation" slot; the storage lane is a single connection, so a second creation would queue anyway.
- **No percentage for create/restore**: the service exposes no progress for them; the page shows a spinner and the elapsed time. Start progress (`VmSummary.start`) is shown with a percentage.
- **Times in UTC** (no time-zone database in the UI).
- `install_media_pending` on the Dashboard (F2) also relies on `VmSummary.run_id` of a stopped VM, which the service does not report: the *Remove installation media* hint may not appear after a stop. Not changed here (outside the baseline scope), reported.
- No snapshot image of the start progress line (covered by a kittest on the accessibility tree).

## Not executed

- Baseline of an installed Ubuntu through the UI; Emergency Stop pressed on the page during a real multi-minute creation (covered at service level in `F3-01` with a fake 8 s convert, and in the UI with the stand-in).
- Linux; a human user at the physical screen and keyboard (screen reader not tried).

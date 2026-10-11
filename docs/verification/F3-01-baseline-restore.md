# F3-01 — Baseline with the VM off, restore, crash reconciliation and test fixture (backend)

- Execution date (UTC): 2026-10-10 / 2026-10-11
- Executed by: development agent "backend F3" (Claude Code session), F3 work split (the baseline screen of the UI is a separate agent)
- Roadmap step: section 10 F3 ("Baseline e recupero"): baseline offline, overlay, manifest and firmware state; locks, journal, reference checks and catalog quotas; recovery after interruption and prudent cleanup of temporaries; refusal to modify disks in use and to delete referenced bases; audit of storage operations. Section 8 (snapshot/restore model of the MVP). ADR-0019 implementation "by F3".
- Related ADRs: ADR-0005, ADR-0008, ADR-0017, ADR-0019, ADR-0025 (no ADR modified here; deviations listed at the end)
- Overall outcome: **partial** — see the results table. Exit criterion items: 30 cycles **verified** (second run 30/30; the first run had 29/30, one guest kernel panic, reported below); base hash unchanged **verified** (60 cycles, two runs); crash tests at the critical points **verified** (injected crashes for every point, real process kill for creation, convert, restore/overlay discard, deletion, and a real kill of the service during a real `qemu-img convert`); no loss of the original disk **verified** (all crash tests and the cycles). Not executed: Linux, the installed-Ubuntu path, a user driving the baseline screen.

Allowed outcomes: **verified**, **failed**, **not executed** (section 17). Raw logs and screenshots are in `lab/f3/` (`logs/`, `evidence-*`), outside the repository.

## Purpose

1. Baselines with the VM off as specified in ROADMAP 8 / ADR-0005: standalone, read-only, hashed image of the VM's current disk plus its UEFI variables, with a versioned manifest, through the storage journal; restore to a new QCOW2 overlay with a fixed internal backing reference plus a fresh copy of the baseline's variables; deletion only if not referenced.
2. Crash safety of every new journal operation (creation, restore with the discard of the previous state, deletion) with reconciliation at the next open; no orphan files, no loss.
3. Disks in use cannot be modified; a tampered baseline is detected (hash through the guard handles); referenced or protected baselines cannot be deleted; no baseline while the VM runs.
4. The deterministic fixture of ADR-0019, produced reproducibly from the repository and imported as an approved data medium (ADR-0008).
5. The F3 exit criterion with real QEMU: 30 cycles of modification in the guest and restore, base hash unchanged.
6. The IPC contract for the UI (additive, `lightvm-proto`).

It does not demonstrate: the UI baseline screen (other agent), agent tools for snapshot/restore (F4/F5), Linux, an installed Ubuntu baseline (see "Choice of the guest path"), application consistency of a guest after a forced stop.

## Environment

| Item | Value |
| --- | --- |
| Host | Intel Core i9-14900K (32 logical processors), 63.8 GiB RAM, system SSD (25–46 GiB free during the session) |
| OS and build | Microsoft Windows 11 Pro 10.0.26300.9550 |
| Accelerator | WHPX |
| Toolchain | Rust 1.99.0 `x86_64-pc-windows-msvc`, MSVC (Visual Studio 2022, wrapper `cargo-msvc.cmd`), `CARGO_TARGET_DIR=target\f3`; real tests in `--release` |
| QEMU | 11.1.0 portable `lab/qemu/qemu-11.1.0/`: `qemu-system-x86_64.exe` SHA-256 `47d57a6072e0bb3bd98f87926eb129eb1736dfe818c67b3b81ef7ce4edd0b3cd`, `qemu-img.exe` SHA-256 `9d930582d0d574bb106d89982c547688e10b8f356192d302cc3d0ae4e8674534` |
| Firmware | `share/edk2-x86_64-code.fd`, variables template `share/edk2-i386-vars.fd` (as `F1-03`) |
| Guest | `lab/media/ubuntu-26.04.1-desktop-amd64.iso`, SHA-256 `601e30fb…1bda1f` (`F0-03`), **live session** |
| Fixture medium | built from `fixtures/lightvm-fx/fx.sh` by `lightvm-fx-iso`: 45 056 bytes, SHA-256 `31599ab9f23fb9b6afec9bb3f2cd0cf1ef92cc88daa70937e879a2a858b09f2d`, volume label `LIGHTVM_FX` (two builds byte-identical) |
| VM profile (cycles) | `pc-q35-11.1`, `-cpu Skylake-Client-v4` (deviation D-1 of F2), 4 vCPUs, 4096 MiB, 8 GiB qcow2 data disk (`/dev/vda`, empty), install medium = Ubuntu ISO, data medium = fixture ISO, offline |

## What was implemented

### Model (ROADMAP 8, adapted to the F2 storage)

- **Current state** of a VM: either the standalone working disk and UEFI variables of F2 (`vms/<vm>/primary.qcow2`, `vms/<vm>/firmware/primary.vars.fd`, produced by the backend, not registered) or, after a restore, an **overlay** `vms/<vm>/overlays/<disk_id>.qcow2` backed by a baseline plus `vms/<vm>/firmware/<fw_id>.vars.fd`, registered in the new table `vm_current_state` (schema v6). The overlay persists across runs until the next restore (as the brief asks), each run references it (`runs.overlay_disk_id`, `runs.baseline_id`, `runs.vars_fw_id`).
- **Baseline creation** (`create_baseline`): VM `stopped`, no open run; source = current overlay (its base guarded too) or standalone disk; source files opened with `FILE_SHARE_READ` only for the whole operation (writer detector, nobody can modify them meanwhile); intents `promote_baseline` + `copy_vars` (journal); on a worker thread: `qemu-img convert -f qcow2 -O qcow2` into the journal temporary (backing chain flattened: **no chains**), `qemu-img compare` source/result (ADR-0025 point 6, an interrupted convert passes `check`), `qemu-img check`, SHA-256 of image and variables; on the supervisor thread: publication without overwrite, read-only attribute, manifest (`format_version` 1, hashes), one transaction for disk, firmware, baseline rows. A VM that never started gets its UEFI variables from the firmware template first, so every baseline carries its firmware state.
- **Clean shutdown / verified**: the backend reports the cause of every run's end (`ExitCause::GuestShutdown` = clean, anything else = not clean), stored in `runs.clean_exit` (v6). A baseline is created as verified only if the VM never ran or its last run ended with an observed guest shutdown; otherwise `create_baseline` is refused (`state_conflict`, `reason` = `unclean_shutdown`) unless `allow_unclean` is given, and then it is recorded **not verified** (`clean_shutdown: false`) for good.
- **Restore** (`restore_baseline`): VM `stopped`, no open run; baseline guarded (`FILE_SHARE_READ` only) and **fully re-hashed through the guard handles** on the worker thread; new overlay with `qemu-img create -f qcow2 -u -b ../base/<baseline_id>/disk.qcow2 -F qcow2` (the only backing reference LightVM writes: fixed shape, one level, relative to the overlay's final directory; the header is re-read by the storage and must name exactly that reference and format); fresh copy of the baseline variables from the guarded handle; previous state discarded with new `discard_file` intents; one transaction for the new rows, `vm_current_state`, deletion of the old overlay rows. The old state is always discarded ("keep" is not offered in F3). A successful restore closes the control sessions of the VM (`TerminationCause::Restored`, for F4) and nothing of the old run can be pending (the VM must be stopped with an empty queue).
- **Start on the overlay**: before QEMU is launched the catalog verifies the base again (full SHA-256 through the guard handles, ADR-0025 point 5), checks the overlay header (fixed reference, `qcow2`) and that the base has no backing file, and keeps the guard (base and baseline variables `FILE_SHARE_READ` only, overlay and variables not deletable) until the run is released. QEMU receives the base as an **explicit read-only `qcow2` node** and the overlay with `backing=` that node: the reference in the header is never used to open it. A refusal there fails the start **before** any process and leaves the VM `stopped` (not `failed`).
- **Deletion** (`delete_baseline`): VM `stopped`; refused if protected (`reason` = `baseline_protected`) or referenced by the current state, an overlay or an open run (`reason` = `baseline_referenced`); one `delete_file` intent per file (image, variables, manifest), group roll-forward like the VM deletion. Ended runs do not keep a baseline or an overlay alive: their references are cleared when the object goes.
- **Budget**: before a creation, the estimated size (source and, for an overlay, its base) is checked against `limits.storage_budget_gib` of the VM (bytes of the VM besides the standalone disk) and against the free space of the catalog volume with a 1 GiB reserve for manifests and the minimal outcome (`resource_limit`).
- **Serialization**: one storage operation per VM in the core (`vm_busy` for every other mutation while it runs, `VmBaselines.operation` visible), journal intents serialized per VM by the catalog, `&mut Catalog` on the single supervisor thread.
- **Cancellation**: the worker polls the Emergency Stop generation, the revocation/closing of the requesting connection and the service shutdown; `qemu-img` runs in its own Job Object with `KILL_ON_JOB_CLOSE` and is terminated within 100 ms; the result is discarded (intents aborted, temporaries removed) and audited as failed. The service shutdown waits (bounded, 60 s) for running jobs.
- **Audit**: `baseline_create`, `baseline_restore` (`accepted` then `succeeded`/`failed`, or `denied`), `baseline_delete` (`succeeded`/`failed`/`denied`), with `vm_id` and `operation_id`, never paths or labels.
- **Identifiers**: new VMs are stored under the core's identifier (`insert_vm_with_id`): all files of the VM live under one `vms/<vm_id>/`. VMs created before F3 keep the two directories (the deletion already removes both).

### Code

| Crate | Changes |
| --- | --- |
| `lightvm-proto` | IPC (additive): requests `list_baselines`, `create_baseline`, `restore_baseline`, `delete_baseline`; responses `baselines`, `baseline_created`, `baseline_restored`, `baseline_deleted`; `VmSummary.baselines` (`VmBaselines`: current, count, bytes, overlay_bytes, operation); `BaselineSummary`; `StorageOperation`; event `baselines_changed`; audit actions `baseline_create`/`baseline_restore`/`baseline_delete`; schemas regenerated |
| `vm-storage` | migration `0006_baseline_state.sql` (journal kind `discard_file`, `vm_current_state` with consistency triggers, `runs.clean_exit`, `baselines.derived_from`/`config_revision`); module `baseline.rs` (three-step jobs for creation and restore, deletion, start guard of the current state, QCOW2 header reader without `qemu-img`, overviews, fault injection for crash tests); reconciliation of `discard_file` (roll back unless committed) and of group deletions of a baseline (roll forward once a file moved); guard verification through the handles |
| `vm-core` | `begin_storage_operation` / `finish_storage_operation` (user only, VM stopped and free, VM busy meanwhile, audit, sessions closed after a restore), `delete_baseline_with`, `refuse_start`, `VmStatus.storage_operation`, busy checks in submit/update/delete |
| `qemu-backend` | `image.rs` (`qemu-img` convert/compare/check/create/info in a Job Object, cancellable); `ManagedPaths::disk_backing` and the explicit read-only base node in the command line |
| `lightvm-service` | engine ports `BaselinePort`, `ImageTool`, `StartDisks`, storage jobs; SQLite adapter; QEMU adapter (`QemuImageTool`, start on the overlay with the catalog guard, end causes); supervisor (worker thread, cancellation, events), server (dispatch, long replies detached); fakes (`FakeImageTool`) |
| `lightvm-fixture` (new) | reproducible ISO 9660 writer, `fx.sh` embedded, pixel verifier of ADR-0019, binary `lightvm-fx-iso` |
| `fixtures/lightvm-fx/` (new) | `fx.sh`, `build.ps1`, `.gitattributes` (LF) |
| `apps/desktop` | only `tests/common/mod.rs`: `baselines: Default::default()` in a `VmSummary` literal (allowed by the brief) |

## Results

| Item | Outcome | Evidence |
| --- | --- | --- |
| IPC contract additive, schemas regenerated, workspace builds (desktop included) | verified | `schema_snapshot`, `cargo check --workspace --all-targets` |
| Creation / restore / creation from an overlay (standalone, derived) / deletion, storage level | verified (synthetic files) | `vm-storage/tests/baseline.rs::create_restore_and_create_from_an_overlay`, `deletion_refuses_referenced_and_protected_baselines`, `ended_runs_do_not_keep_a_baseline_or_an_overlay_alive` |
| Same through IPC with the real core and SQLite catalog (fake backend and image tool) | verified | `lightvm-service/tests/f3.rs::create_restore_start_on_the_overlay_and_delete` (start uses the overlay files, events, audit counts, VM deletion with baselines) |
| Baseline/restore/delete refused with the VM running | verified | `baseline.rs::baseline_and_restore_require_a_stopped_vm`; `f3.rs` (`state_conflict` while running) |
| Unclean stop: refused without consent, recorded not verified with it | verified | `f3.rs::unclean_stop_requires_explicit_consent` (forced stop → `unclean_shutdown`; graceful stop → verified) |
| VM busy during a long operation; Emergency Stop cancels it, nothing published | verified (fake tool with an 8 s convert) | `f3.rs::vm_is_busy_during_a_long_operation_and_the_emergency_stop_cancels_it` (cancelled in < 5 s, `tmp/` empty, audit `failed`) |
| Tampered baseline detected at start (VM stays stopped) and at restore | verified | `baseline.rs::tampered_baseline_is_detected_at_restore_and_at_start`; `f3.rs::tampered_baseline_refuses_the_start_and_the_vm_stays_stopped` (`reason` = `integrity`) |
| Disk in use / writer detector: creation and restore refused while another program holds the disk or the base open for writing; source not writable during a creation | verified (Windows share mode) | `baseline.rs::writer_detector_refuses_baseline_and_restore` |
| Deleting a referenced baseline refused | verified | `baseline.rs`, `f3.rs` (`reason` = `baseline_referenced`) |
| UI-only requests (pairing role refused), not found | verified | `f3.rs::baseline_requests_are_refused_for_the_pairing_role` |
| Core: busy, user only, sessions closed after a restore, start refused before the backend | verified | `vm-core/tests/f3_baselines.rs` (4 tests) |
| Crash at every critical point (injected, catalog dropped without rollback, reopened) | verified | `baseline.rs::crash_during_baseline_creation_…` (4 points), `crash_during_restore_…` (5 points × from standalone / from overlay), `crash_during_baseline_deletion_…` (3 points), `interrupted_jobs_are_rolled_back_at_the_next_open` |
| Real process kill (TerminateProcess of a child holding lock, SQLite and handles) during convert, creation, restore (overlay discard), deletion | verified | `vm-storage/tests/process_kill.rs` (section "Crash tests") |
| Real kill of the **service** during a real `qemu-img convert` of a 3 GiB disk | verified | `f3_real.rs::e2e_real_kill_service_during_baseline_convert` |
| Explicit backing node in the QEMU command, audited options | verified | `qemu-backend` `cmdline::tests::overlay_with_explicit_backing_node` |
| Fixture ISO reproducible, verifier | verified | `lightvm-fixture` tests; two builds identical (SHA-256 above) |
| Exit criterion: 30 cycles with real QEMU | **verified** (run 2: 30/30, no retry); run 1: **failed** in 1 cycle of 30 (guest kernel panic after the restore check) | section "Exit criterion" |
| Base hash unchanged across the cycles | verified (image and UEFI variables, after each of 60 cycles) | section "Exit criterion" |
| Installed Ubuntu baseline | not executed | see "Choice of the guest path" |
| Linux | not executed | — |

### Test counts (this session, all green)

`cargo test -p lightvm-proto -p vm-storage -p vm-core -p qemu-backend -p lightvm-service -p lightvm-fixture` (debug): every suite passes; new tests: `vm-storage` `baseline.rs` 11, `process_kill.rs` 1 (4 kill points), lib `qcow2_backing_reference_is_read_from_the_header`; `vm-core` `f3_baselines.rs` 4; `lightvm-service` `f3.rs` 5, `f3_real.rs` 2 (ignored, real QEMU); `qemu-backend` 2 (`fixed_backing_reference_shape`, `overlay_with_explicit_backing_node`); `lightvm-fixture` 2. The suites of `lightvm-desktop` (only change: the `VmSummary` literal), `lightvm-mcp` and `rfb-client` also pass. `cargo clippy --all-targets -- -D warnings` clean on the six crates and on `lightvm-service --no-default-features`; `cargo fmt` applied to them.

## Crash tests

### Injected (fault points, `FaultAction::Crash`: the operation stops without rollback, the catalog is dropped and reopened)

| Operation | Point | After reconciliation |
| --- | --- | --- |
| creation | image produced, nothing published | no new baseline, `tmp/` empty, current state unchanged |
| creation | disk published, variables not | same (published disk and directory removed) |
| creation | files and manifest published, not committed | same |
| any | committed, temporaries not yet removed | baseline complete; leftover temporaries removed |
| restore (from standalone and from overlay) | overlay produced, nothing published | old state intact |
| restore | new overlay and variables published | new files removed, old state intact |
| restore | first old file discarded (overlay discard in progress) | discarded file put back, new files removed |
| restore | old state discarded, not committed | old state back, new files removed |
| restore | committed | new state complete, old files removed |
| deletion | first file moved | rolled forward: rows deleted, remaining files removed |
| deletion | all files moved, not committed | rolled forward |
| deletion | committed | complete |

Each case also checks: no open intent, `tmp/` empty, every file under `vms/<vm>/` registered (no orphans), base hashes unchanged, VM not left busy (a new operation succeeds).

### Real process kill (`process_kill.rs`)

The test binary re-runs itself as a child that performs the operation and blocks at the point (`FaultAction::Pause` writes a marker); the parent checks that the catalog is locked by the child, kills it (`TerminateProcess`) and reopens:

```text
convert: killed in 6.8ms; reconciliation: [(PromoteBaseline, RolledBack), (CopyVars, RolledBack)]
create:  killed in 5.3ms; reconciliation: [(PromoteBaseline, RolledBack), (CopyVars, RolledBack)]
restore: killed in 4.7ms; reconciliation: [(CreateOverlay, RolledBack), (CopyVars, RolledBack), (DiscardFile, RolledBack), (DiscardFile, RolledBack)]
delete:  killed in 6.4ms; reconciliation: [(DeleteFile, RolledForward), (DeleteFile, RolledForward), (DeleteFile, RolledForward)]
```

Outcome **verified**: base unchanged, the restore interrupted in the middle of the overlay discard put the old overlay back, the deletion completed, no orphans, a full restore succeeds afterwards.

### Real kill of the service during a real `qemu-img convert` (`f3_real.rs`, log `lab/f3/logs/kill-2.log`)

A 3 GiB qcow2 with pseudo-random data (SHA-256 `4a56de47…3de15`) is placed as the standalone disk of a VM of a child **service** process (real backend, real `qemu-img`); the parent kills the service as soon as the baseline temporary exceeds 256 MiB:

```text
service killed during the convert: partial image 277151744 bytes after 1.01634s
reconciliation: [(PromoteBaseline, RolledBack, "temp_removed"), (CopyVars, RolledBack, "nothing_to_undo")]
baseline after the crash in 9.929489s: 3222487040 bytes, sha256 4a56de47…3de15
restore of a 3 GiB baseline (full hash verification): 2.4317342s
start on the overlay of a 3 GiB baseline (verification + QEMU start): 2.0966935s
```

`qemu-img` died with the service (Job Object kill-on-close: the partial file could be opened for writing right after); after the reopen: no baseline, no temporary, no open intent, standalone disk hash unchanged; a new baseline through a new service succeeds (convert + compare + check + hashes of 3 GiB in 9.9 s), the restore (full re-hash of the base through the guard) takes 2.4 s, a start on the overlay 2.1 s including the second full verification. A first run (`kill-1.log`) found a defect, fixed and re-run: a VM that never started had no UEFI variables, so its baseline had none and the start on the overlay was refused; baselines now always carry the variables (template for a VM that never started).

## Exit criterion: 30 cycles with real QEMU

### Choice of the guest path

The **live session + empty data disk** path of ADR-0019 ("Environment and initial state") was used, as in F0-05, not an installed Ubuntu: the installed guest of `F1-17`/`F2` requires logging in with the credentials of `lab/vms/f1-17/credenziali-guest.txt`, which this agent must not read, and ADR-0019 defines the fixture on the live session. Validity: the cycle exercises exactly the product path that matters for F3 — the VM's current state is the 8 GiB `/dev/vda` disk; the baseline is a standalone image of it plus the UEFI variables; every cycle writes the marker into the overlay from the guest (`dd … conv=fsync`), stops the VM (alternately forced stop and guest power-off), restores (new overlay + variables copy, old overlay discarded), boots and checks from the guest that the marker is gone. What it does not cover: a multi-GiB installed root filesystem as baseline (covered for size/time only by the 3 GiB real convert above) and the boot of a guest from the overlay disk (the live session boots from the ISO; the disk is a data disk).

### Procedure

`f3_real.rs::e2e_real_qemu_f3_baseline_restore_cycles` (release, `LIGHTVM_F3_CYCLES=30`), everything through the real service and IPC: import of the Ubuntu ISO (install) and of the fixture ISO (data, built in the test with `lightvm_fixture::fixture_iso()`), VM creation, first boot; A0 readiness (two captures 5 s apart identical with ≥ 99 % lit pixels, timeout 300 s); reference screenshot without fixture colours (calibration guard); A3 (Ctrl+Alt+T, screen changed within 15 s, then settled); A4 `test -f /mnt/fx.sh || sudo mount -r -L LIGHTVM_FX /mnt; sh /mnt/fx.sh mostra` typed through the service input path (US layout); V3 with marker absent; clean power-off from the guest (`sudo systemctl poweroff -i`, Enter for the casper prompt); `create_baseline` (verified, never forced); then per cycle: `restore_baseline` → start → A0 → `mostra` (**V6**: marker absent) → `scrivi` (**V4**: marker present) → stop (even cycles forced, odd cycles guest power-off) → on cycles 1, 10, 20, 30 a start **without** restore → `mostra` (**V5**: marker still present) → forced stop → SHA-256 of the base image and of the baseline variables on the host. Screens are read from the console ticket's RFB framebuffer (lossless 32-bit pixels) and checked with `lightvm_fixture::verify` (exact colours, full connected boxes, B1…B4 aligned and ordered, B5 state). A fixture command whose output is not found is retried once with a new terminal (counted).

### Results

Two complete runs of 30 cycles each (same binary except the per-cycle retry counter in the log line), logs `lab/f3/logs/cycles-30.log` (run 1) and `lab/f3/logs/cycles-30b.log` (run 2), screenshots `lab/f3/evidence-30/`, `lab/f3/evidence-30b/` (reference, first boot, cycles 1–2 before/after the write, every power-off and every failure).

Common facts of both runs:

- reference screenshot of the live desktop without any fixture colour (calibration guard passed), first `mostra` on the empty disk: V3 with marker absent;
- clean power-off from the guest observed (`ExitCause::GuestShutdown`): the baseline was created **verified** (`clean_shutdown: true`), 737 792 bytes, in 107–114 ms, disk SHA-256 `e80c945307d0738d449e58062253eeecf7a0f9c3f1a49ed6bacede39463014d2` — identical in both runs and in the smoke run (the convert of the same empty 8 GiB disk is deterministic);
- stops alternated: 15 forced stops and 15 guest power-offs per run.

| Item | Run 1 (`cycles-30.log`) | Run 2 (`cycles-30b.log`) |
| --- | --- | --- |
| Cycles run | 30 | 30 |
| V6 (marker absent after restore) | **30/30** | **30/30** |
| V4 (marker present after `scrivi`) | 29/30 (cycle 12: not evaluable, guest kernel panic) | **30/30** |
| V5 (marker persists across a restart without restore; cycles 1, 10, 20, 30) | 4/4 | 4/4 |
| Fixture retries (output not found, new terminal) | 2 | 0 |
| Base image and baseline variables SHA-256 after every cycle | unchanged 30/30 | unchanged 30/30 |
| Restore duration min / median / max | 63 / 67 / 123 ms | 63 / 71 / 79 ms |
| Desktop ready after start min / median / max | 120 / 125 / 130 s | 120 / 125 / 130 s |
| Audit | `baseline_restore` succeeded 30, `baseline_create` succeeded 1 | same |
| Test outcome | **failed** (1 failure) | **passed** |

Run 1, cycle 12: after the restore and a successful V6 (marker absent), the live guest crashed while the next fixture command was being typed: the screen shows `KERNEL PANIC! … Attempted to kill init! exitcode=0x0000000b` (systemd killed by SIGSEGV, `evidence-30/f3-12-failed.png`). The QEMU process, QMP and storage were fine; the restore of cycle 13 brought the VM back to the baseline and the remaining 18 cycles passed. Classified as a guest failure during the cycle (ADR-0019 outcome `failed` for that cycle), not a storage or restore failure; the cause inside the live guest (CPU model `Skylake-Client-v4` under WHPX, the live session itself) was not investigated. Second run without any failure or retry.

**Outcome of the exit criterion: verified** with the second run (30 consecutive cycles of modification in the guest and restore with return to the initial state, base hash unchanged); the first run is kept as a failed attempt (one guest crash in 30 cycles).

## Negative tests (summary)

| Negative | Outcome |
| --- | --- |
| Modifying a disk in use: another program holds the working disk open for writing → creation refused (`vm_busy`); the base open for writing → restore refused; during a creation the source cannot be opened for writing by others; during a run the base and baseline variables are open `FILE_SHARE_READ` only (ADR-0025, as F0-05 §7.2) | verified |
| Deleting a referenced baseline → `state_conflict` `baseline_referenced`; protected → `baseline_protected` | verified |
| Baseline / restore / deletion with the VM running → `state_conflict` | verified |
| Baseline file tampering → start refused before QEMU (`state_conflict` `integrity`, VM stays `stopped`), restore refused, nothing changed | verified |
| Baseline after an unclean stop without consent → `state_conflict` `unclean_shutdown` | verified |

## Measurements

| Operation | Value |
| --- | --- |
| Baseline of the 8 GiB empty data disk (convert + compare + check + hashes) | ~135 ms (smoke run, `lab/f3/logs/smoke-4.log`) |
| Baseline of a 3 GiB disk full of data | 9.9 s |
| Restore of a 3 GiB baseline (full SHA-256 of the base through the guard) | 2.4 s |
| Start on the overlay of a 3 GiB baseline (second full verification + QEMU start) | 2.1 s |
| Restore of the fixture baseline (cycles) | 63 / 67–71 / 79–123 ms (min / median / max, two runs) |
| Live desktop ready (A0) after start | 120 / 125 / 130 s (min / median / max) |

The full verification at every start costs about 0.8 s per GiB of base on this host (SHA-256 at ~1.3 GB/s); it runs on the supervisor thread (other VMs wait meanwhile). An optimisation (verified identity cache) is left for later and would need an ADR-0025 decision.

## Deviations and decisions to record

1. **Overlay per current state, not per run.** ROADMAP 8 point 4 says "each test uses a QCOW2 overlay"; the brief asks that runs after a restore keep using that overlay until the next restore. Implemented as the brief: one overlay per current state, referenced by every run on it. A per-test overlay remains possible on top (the old `prepare_run_from_baseline` API is still there) and belongs to F4 sessions.
2. **The previous state is always discarded** on restore ("keep" not offered). The standalone F2 disk is discarded at the first restore.
3. **Fixed backing reference** `../base/<baseline_id>/disk.qcow2` relative to the overlay, verified from the header by the storage; QEMU gets the base as an explicit node (the header path is not used at run time). F0-05 proposed `..\baseline\base.qcow2` with `qemu-img info --backing-chain`; the header is now read by the storage itself (no process), and the base is checked to have no backing file.
4. **ADR-0019 changes**: `$((3**4))` is written `$((3*3*3*3))` (`sh` is `dash`, no `**`, same result 81); the medium is mounted by label at `/mnt` when not already there (the automatic mount at `/media/ubuntu/LIGHTVM_FX` is not relied upon); pixels come from the RFB framebuffer of the console ticket instead of `screendump` PPM (both lossless); V1/V2 (mouse on the welcome window) not implemented (not needed for the restore criterion; F5 workload); CPU `Skylake-Client-v4` and 4 vCPUs instead of `qemu64`/2 (F2 deviation D-1: the live desktop does not start with `qemu64`); minimum cell size of the calibration fixed at 6×12 px (observed cells 8×18 px at 1280×800); clean power-off of the live session needs `systemctl poweroff -i` (inhibitors) and Enter for the casper prompt.
5. **Protected baselines**: the flag exists in storage and is honoured (deletion refused); the UI cannot create protected baselines in F3 (they are the initial baselines of agent sessions, F4).
6. **Budget**: the VM budget (`limits.storage_budget_gib`) and the free-space reserve are checked before a creation; the catalog-wide budgets of `storage_budgets` (ADR-0017) are not applied to baselines yet.
7. **Agents**: all baseline requests are user only in F3; the core denies agent callers (`permission_denied`) until F4/F5 define `vm.snapshot`/`vm.restore`.
8. Proposed for ADR-0005 / ADR-0025: record points 1–3 above and the "verified only after an observed guest shutdown" rule as implemented (`runs.clean_exit`).

## Limits

- Windows only; share-mode protections do not exist on Linux (declared, ADR-0016/0025).
- The overlay in use stays writable by other processes of the same user (ADR-0025 point 4, declared limit, unchanged).
- A crash between the commit of an operation and the rewrite of the VM manifest leaves the VM manifest stale (`verify_vm_manifest` reports it); it is rewritten at the next operation on the VM, not by reconciliation.
- The temporary of a creation is created by `qemu-img` itself (not `CREATE_NEW`): its name is a fresh UUID in the catalog's `tmp/`, checked absent right before.
- Synthetic fixture images in the headless tests are files with a QCOW2 header, not real images; real images are covered by the real tests only.
- The live-session cycles do not boot a guest from the overlay disk (data disk only).

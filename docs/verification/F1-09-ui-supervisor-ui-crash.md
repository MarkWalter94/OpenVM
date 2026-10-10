# F1-09 — UI that starts the supervisor as a separate process and UI crash with a real VM (P-2)

- Execution date (UTC): 2026-10-10, from about 09:05 to 09:20
- Executed by: development agent (Claude Code session), at the request of the project owner
- Roadmap step: section 6.1 (supervisor separate from the UI), 7.3 (parent loss), 10 F1 ("supervisor skeleton"); backlog `docs/RELEASE_PLAN.md` F1-11; F0 pending item **P-2** (`docs/STATUS.md`)
- Related ADRs: ADR-0002, ADR-0015 (part B pending), ADR-0024 (not modified)
- Overall outcome: **partial** — UI → child supervisor → IPC verified on Windows with an unconfigured backend and with real QEMU; UI crash with a real VM **verified without orphan processes**, but the orderly shutdown of the VM **does not complete** within the supervisor's shutdown timeout (see Results and Proposed decisions); Linux not executed

Allowed outcomes: **verified**, **failed**, **not executed** (section 17). Raw logs in `lab/logs/` (outside the repository): `p2-crash-ui.txt`.

## Purpose

1. The UI (`lightvm-desktop`) starts `lightvm-service` as a child process, reads the `ready` line from stdout (private anonymous pipe), authenticates via IPC with the credential received and shows the VMs of the service's catalog; start, stop and Emergency Stop go through the service's core. No access to QMP or to VM files from the UI on this path.
2. The UI does not link `qemu-backend`, `vm-core` nor `vm-storage` (ADR-0024).
3. P-2: with a real VM running, the abrupt termination of the UI (`TerminateProcess`) leads the supervisor to detect the parent loss, apply the default policy (`shutdown`) and exit, without orphaned QEMU processes.

It does not demonstrate: the VM console through the supervisor (the F0-04 spike console still opens the QMP socket from the command line; the per-run display transport is F2, ADR-0013); a UI crash during a start or stop in progress; the `keep-manual-vms` policy; Linux; a second user.

## Environment

Same host, toolchain and QEMU as `F1-03-real-qemu-backend.md` (Windows 11 Pro 10.0.26200, i9-14900K, Rust 1.99.0 MSVC, QEMU 11.1.0 with WHPX). Catalog, runtime directory and audit file in per-test temporary directories (`%TEMP%\lightvm-desktop-*`), passed with `--catalog`, `--runtime-dir`, `--audit-log`.

## Changes tested

- `crates/lightvm-service`: feature `supervisore` (default) that encloses the composition root, server and adapters; without it `client`, `credential`, `frame`, `transport` remain. The service's binaries and integration tests require the feature.
- `apps/desktop/src/service.rs`: start of the child with `--parent-watch stdin`, reading the `ready` line with a timeout (30 s), two IPC connections (requests and events: reading a frame cannot be safely interrupted), commands `Aggiorna`, `Crea` (not exposed in the widgets), `Avvia`, `Arresta`, `EmergencyStop`; controlled shutdown with the `shutdown` line and forced termination after 15 s.
- `apps/desktop/src/lib.rs`: "Supervisor" panel in the Dashboard (`servizio_ui`, pure function testable without processes).
- `apps/desktop/src/main.rs`: options `--catalogo`, `--servizio-exe`, `--qemu-dir`, `--runtime-dir`, `--audit-log`; the supervisor starts only once the window is created (failed rendering attempts leave no services running); in the smoke, lines `SMOKE servizio …` and `SMOKE servizio_arresto …`.

## Procedure

```text
cargo build -p lightvm-service --bins
cargo test -p lightvm-desktop --lib --test servizio_ui --test servizio_figlio
cargo tree -p lightvm-desktop -e normal --prefix none   (filtered on qemu-backend, vm-core, vm-storage, rusqlite)

set LIGHTVM_QEMU_DIR=<workspace>/lab/qemu/qemu-11.1.0
cargo test -p lightvm-desktop --test servizio_figlio -- --ignored --nocapture crash_ui_con_vm_reale
```

The P-2 test (`crash_ui_con_vm_reale`) relaunches its own test binary as the "UI" process: this starts the supervisor with QEMU, creates a VM (1 vCPU, 1024 MiB, 8 GiB disk, no ISO), starts it, waits for the `running` state and prints the service's PID. The test finds the service's `qemu-system-x86_64.exe` child (`Win32_Process.ParentProcessId`), terminates the "UI" with `Child::kill` (`TerminateProcess`) and checks with `tasklist` every 50 ms, for at most 120 s, the end of QEMU and of the service.

## Results

| Item | Outcome | Evidence |
| --- | --- | --- |
| The UI does not link `qemu-backend`, `vm-core`, `vm-storage`, `rusqlite` | verified | `cargo tree -p lightvm-desktop -e normal`: of these only `lightvm-service` appears (without the `supervisore` feature) |
| Client side of the service compilable on its own | verified | `cargo clippy -p lightvm-service --no-default-features --all-targets -- -D warnings`: no warnings |
| Valid and invalid `ready` line | verified | `servizio::tests::riga_ready_valida`, `riga_ready_non_valida` (wrong JSON, different event, pid, endpoint or credential missing or malformed) |
| Panel: state as text, buttons enabled only if the action is allowed, command returned on click | verified (headless) | `tests/service_ui.rs`, 4 `egui_kittest` tests: `Avvia` only on a `stopped` VM, `Arresta` only on `running`, everything disabled with the service terminated or not connected |
| UI → child supervisor → handshake, subscription, capabilities, VM list, Emergency Stop, controlled shutdown | verified | `ui_avvia_il_supervisor_e_lo_arresta`: state `connesso`, empty catalog, `Emergency Stop: 0 processi …`, exit with code 0 without forced termination |
| Second instance on the same catalog | verified | rejected with exit code 3 (`UscitoPrimaDelPronto(Some(3))`); service stderr: `un'altra istanza … sta già usando questo catalogo` |
| Service executable missing | verified | `ErroreAvvio::Spawn` |
| Create and start a real VM from the UI through the service | verified | in the P-2 test: VM `running` with a `qemu-system-x86_64.exe` child of the service (`UI pid 25224 → servizio pid 936 → QEMU [38500]`) |
| **P-2:** parent loss detected | verified | service stderr: `processo genitore terminato` |
| **P-2:** no orphan processes | verified | QEMU terminated 30.20 s, service 30.08 s after the UI kill; no leftover process |
| **P-2:** orderly shutdown of the VM | **failed** | stderr: `timeout nell'arresto del supervisor`; audit: `vm_stop` `accepted` (seq 7) and `forwarded` (seq 9), **no** `succeeded`. The end of QEMU 0.12 s after that of the service is consistent with the Job Object being closed when the process exits (deduction from the timings, not directly observed), not with the orderly shutdown |
| Linux (lifeline pipe, pidfd) for this path | not executed | — |
| Manual test of the app with a window (`lightvm-desktop --catalogo …`) | not executed | covered only by the headless tests and by the module driven by the tests |

Cause of the orderly shutdown failure: on parent loss the service stops the VMs with the backend's ordinary policy (`ShutdownPolicy::default()`: `system_powerdown` with a 60 s wait, then `quit` 5 s, then kill 10 s). The VM in the firmware does not respond to ACPI. The server's shutdown driver waits for the supervisor for `shutdown_timeout` = 30 s, then releases the lock and the process exits. The service's shutdown budget (30 s) is lower than that of a single VM (up to 75 s).

## Limits

- A single Windows host; a single P-2 sample (plus the two previous attempts described below).
- First valid run preceded by two attempts with defects **in the test**, not in the product: (1) the child's `PRONTO` line was preceded by the libtest prefix and was not recognized; the test ended on a timeout leaving the test UI, service and QEMU alive, which were then terminated by hand (processes gone after the kill of the test UI; times not recorded because of an error in the measurement script); (2) a 30 s waiting window, shorter than the actual time (≈30.2 s). The test now terminates the test UI even in case of a timeout.
- Before `--audit-log` was added, a test created an empty audit file in `%LOCALAPPDATA%\LightVM`; the file and the directory were removed.
- The release build (`cargo build --release -p lightvm-desktop`) does not produce `lightvm-service`: the installer will have to distribute both (the UI looks for it next to its own executable or with `--servizio-exe`).

## Required actions

1. Decide the shutdown budget on UI loss (ADR-0015 part B): see Proposed decisions.
2. Repeat P-2 after the decision, with at least 5 samples and with a guest that responds to ACPI (F1-17).
3. Run the test `ui_avvia_il_supervisor_e_lo_arresta` on Linux (CI `ubuntu-latest` runs it with `cargo test --workspace`; never run on GitHub).
4. Move the console onto the supervisor's IPC with the per-run display credential (F2, ADR-0013).

## Proposed decisions

- **ADR-0015 part B / shutdown budget:** on UI loss use a dedicated policy for the VMs with a short ACPI wait (for example a few seconds, then `quit` and kill) and make the service's `shutdown_timeout` no lower than the sum of the policy's steps for the number of VMs stopped in parallel; alternatively declare that on UI loss the VMs are terminated with an Emergency Stop. The choice is up to the project owner: today the final result (no orphans) depends, according to the observed timings, on the Job Object being closed when the service exits.


## Update of 2026-10-10 (about 10:10 UTC): policy on UI loss as a setting

Section appended at the end (rule 8 of `docs/verification/README.md`): the outcome "orderly shutdown failed" of the previous section remains recorded. Decision in ADR-0015 part B.

### Changes

- `lightvm-proto`: `ParentLossPolicy` (`kill_vms` default, `shutdown_vms { powerdown_s }` 1–300, `keep_vms`), request `set_parent_loss_policy` (UI only), active policy in `get_capabilities`; schemas regenerated (`parent_loss_policy.json`, `ipc_*`).
- `lightvm-service`: `--parent-loss-policy kill-vms|shutdown-vms|keep-vms` and `--parent-loss-powerdown-s N`; on UI loss `kill_vms` performs the Emergency Stop and then the shutdown, `shutdown_vms` imposes the chosen ACPI wait on the QEMU backend and extends the shutdown timeout to `powerdown_s + 20` s; if the supervisor does not finish within the timeout, Emergency Stop of the remaining VMs before exiting.
- `apps/desktop`: setting in Settings (radio buttons and a menu of wait times, no text field), saved in `impostazioni-ui.json` in the user profile (`--impostazioni FILE` for the tests), passed to the service at startup and sent immediately via IPC if it changes; "lascia le VM accese" disabled in the UI until reattachment exists. In the same Settings section: preferred rendering backend (effective from the next start, see `F1-05`).

### Results

| Item | Outcome | Evidence |
| --- | --- | --- |
| **P-2, default policy `kill_vms`**: VM terminated immediately, service exited, no orphans | verified | `crash_ui_con_vm_reale`: QEMU terminated 105.0 ms and service 208.0 ms after the UI kill; stderr `UI persa, Emergency Stop: 1 processi terminati, completato true`; audit `emergency_stop` `accepted` → `succeeded` |
| **P-2, `shutdown_vms` with 3 s**: orderly shutdown completed, no timeout | verified | `crash_ui_con_vm_reale_spegnimento_ordinato`: QEMU 3.21 s, service 3.29 s; audit `vm_stop` `accepted` → `forwarded` → `succeeded`; no `timeout nell'arresto del supervisor` |
| Three policies in the service (fake backend) | verified | `ipc.rs`: `parent_lost_in_process_stops_agent_sessions_then_shuts_down` (Emergency Stop before the shutdown), `parent_lost_with_shutdown_vms_stops_without_emergency`, `parent_lost_with_keep_vms_leaves_the_service_running` |
| `set_parent_loss_policy` only from the UI, validation of the limits, policy in `get_capabilities`, effective at the next loss | verified | `ipc.rs::parent_loss_policy_is_set_via_ipc_by_the_ui_only` (pairing → `permission_denied`; `powerdown_s: 0` → `invalid_argument` without effect) |
| JSON shape, default, compatibility with capabilities lacking the field | verified | `lightvm-proto` `ipc::tests::parent_loss_policy_wire_shape_default_and_limits` |
| Setting in Settings saved to a file; file missing, partial or invalid | verified (headless) | `kittest_ui::settings_salva_politica_e_rendering`; `impostazioni::tests` (6 tests, including `keep_vms` from the file → default with a warning) |
| `keep_vms` role with real QEMU | not executed | disabled in the UI |

### Limits

- One sample per policy, a single Windows host; Linux not executed.
- With `kill_vms` the state of the running VM's overlay is lost (Emergency Stop behaviour, section 7.3); the baseline is not touched.
- `shutdown_vms` tested with the firmware, which does not respond to ACPI (escalation to `quit`); a real guest that powers off within the wait was not tested.

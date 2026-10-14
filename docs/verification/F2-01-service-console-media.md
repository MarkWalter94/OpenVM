# F2-01 — Supervisor for the manual alpha: media registry, VM deletion, console ticket and input

- Execution date (UTC): 2026-10-10
- Executed by: development agent "service" (Claude Code session), F2 work split
- Roadmap step: section 10 F2 (import ISO through the user's picker, disk management, console with input, graceful stop with timeout / forced stop / reset, deletion); sections 6.3, 6.4, 7.4 (rows "Host", "File", "IPC/console")
- Related ADRs: ADR-0008, ADR-0009, ADR-0012, ADR-0013, ADR-0017 (no decision modified here; proposals at the end)
- Overall outcome: **partial** — every F2 request of the IPC contract is implemented and verified headless (fake backend) and end to end with real QEMU/WHPX and the Ubuntu ISO on the first host; the graceful ACPI stop of the Ubuntu **live** desktop did not complete within the 60 s wait (escalation to `quit`, see Results); the pre-existing F1 real-QEMU test could not run on this host because of free disk space (see Limits)

Allowed outcomes: **verified**, **failed**, **not executed** (section 17). The raw output of the real runs is in the session scratchpad (`e2e-run2.log`, `e2e-run5.log`, `e2e-run6.log`), not in `lab/` (this agent does not write there); the relevant lines are quoted below with user paths shortened to `%TEMP%`.

## Purpose

1. `list_media` / `import_media` (UI role only): checks on the path chosen with the native picker, managed read-only copy through the storage journal (replaced by the file used in place for new imports since ADR-0029: the measurements below concern the managed copy), ADR-0008 size limits, kinds install/data; the imported medium is usable as `install_media`/`data_media` in `create_vm` and verified by the media inspector at start.
2. `delete_vm`: only a stopped VM without queued or forwarded operations; rows and files (disk, run directories with the UEFI variables and logs) removed through the storage journal; audit.
3. `VmSummary.disk_used_bytes` and `Capabilities.host`.
4. Input, screenshot and reset connected to the QEMU backend; input from the console without a prior screenshot while still rejecting stale coordinates; IPC→QMP latency.
5. `open_console` and the per-run display proxy with ticket (ADR-0013 obligation "per-run display credential"), revocation.
6. Graceful stop with timeout, forced stop and reset distinct and working through IPC.

It does not demonstrate: the desktop UI (agent "desktop", `F2-02`), the installation of Ubuntu to disk and the boot from the disk through the service (F2 exit criterion), a second Windows user against the display directory, Linux.

## Environment

| Item | Value |
| --- | --- |
| Host | Intel Core i9-14900K (32 logical processors), 63.8 GiB RAM, system SSD with 9–12 GiB free during the session (see Limits) |
| OS and build | Microsoft Windows 11 Pro 10.0.26300.9550 |
| Accelerator | WHPX (`accelerator Some("whpx")` in `get_capabilities`) |
| Toolchain | Rust 1.99.0 `x86_64-pc-windows-msvc`, MSVC from Visual Studio 2022 Community (wrapper `cargo-msvc.cmd`) |
| QEMU binary | 11.1.0 (`v11.1.0-12130-ge470268ff4`), `qemu-system-x86_64.exe` SHA-256 `47d57a6072e0bb3bd98f87926eb129eb1736dfe818c67b3b81ef7ce4edd0b3cd` (as `F1-03`) |
| Firmware | `share/edk2-x86_64-code.fd` and `edk2-i386-vars.fd` of the same distribution (as `F1-03`) |
| Guest / media | `lab/media/ubuntu-26.04.1-desktop-amd64.iso`, 6 482 409 472 bytes, SHA-256 of the managed copy computed by the import `601e30fbf5d97759367c632e2c33630665039b7e2158fd068403da3ccf1bda1f` (equal to the published one, `F0-03`) |
| VM profile | `pc-q35-11.1`, `-cpu qemu64` (service default, ADR-0011), 2 vCPUs, 4096 MiB, 25 GiB qcow2, offline, install medium = the imported ISO |
| Session | local interactive desktop; tests run from a terminal |

## What was implemented

### IPC requests (contract of commit `2a769fe`, unchanged)

| Request | Behaviour |
| --- | --- |
| `list_media` | approved media of the registry (`media` rows with status `approved`) |
| `import_media` | path checks in the service (`adapters::catalog::validate_import_source`), then `vm_storage::Catalog::begin_media_import` (source opened once with `FILE_SHARE_READ` and without following a reparse point; size checked on the handle; `import_media` intent), copy + SHA-256 + ISO 9660/UDF signature on the copied bytes **on a worker thread** (`MediaImportJob::copy`), publication (`finish_media_import`: read-only copy, hard link without overwrite, registry row and intent commit). Audit `media_import` (never the path) |
| `delete_vm` | core: user only, VM `stopped`, no queued or forwarded operation, expected revision (`state_conflict` / `vm_busy` / `not_found`, audit `vm_delete` denied); storage: `Catalog::delete_vm_with_files` (journal, see below); audit `vm_delete` succeeded or failed |
| `open_console` | core: user only, VM `running` (audit `console_open` with the run, never the token); the display registry issues the ticket and starts the proxy; reply `ConsoleTicket { vm_id, run_id, socket_path, token, geometry }` |
| `submit_vm` input / screenshot / reset | connected to `QemuRun::send_input`, `screenshot_limited` (metadata only over IPC), `reset` |
| `get_capabilities` | `host` = host capacity used by the core (`input_available` and `screenshot_available` now `true` with QEMU) |
| `list_vms` / `vm_status` | `disk_used_bytes` = bytes of `vms/<vm_id>/` and of `runs/<run_id>/` of every run of the VM |

The UI connection is not blocked by an import: `import_media` is served in a detached task and its reply arrives (by `request_id`) when the copy ends, while the same connection keeps receiving events and serving requests. At most 2 imports copy at the same time (`resource_limit` beyond). The import is refused before copying if the catalog volume would keep less than 1 GiB free (`resource_limit`); a full disk during the copy is also `resource_limit`.

### Path checks of `import_media` (ROADMAP 7.4 "Host" row)

Refused: empty or control characters, relative paths, `.`/`..`, UNC (`\\server\share`) and device paths (`\\?\`, `\\.\`, `//?/`, `\??\`), alternate data streams (`:` after the drive), Windows device names (`NUL.iso`, `CON`, `COM1`…), extensions other than `.iso` (case-insensitive), missing files (`not_found`), links/junctions/any reparse point (`permission_denied`), non-regular files (directories), empty files, files over 16 GiB (install) or 8 GiB (data) (`resource_limit`), files without the `CD001`/`BEA01` signature (`invalid_argument`). The storage repeats type, size and signature on the handle it copies from.

### VM deletion in the storage (schema v5)

- Migration `0005_journal_vm_target.sql`: target kind `vm` in the journal (table rebuilt as in 0003).
- `Catalog::delete_vm_with_files(vm, extra_dirs)`: checks (VM `stopped`/`failed`, no open run, no protected baseline), inventory of the regular files under `vms/<vm_id>/`, `runs/<run_id>/` of every run and the extra directories (the service passes `vms/<core id>`); a link or reparse point anywhere aborts before any change; one `delete_file` intent per file with the same operation id; files moved into `tmp/`; one transaction deletes every row of the VM (operations, artifacts, runs, overlays, grants, baselines, disks, firmware, media slots, budget, VM) and commits all intents; directories removed after the commit.
- Reconciliation: once one file of the group was moved the group rolls forward (rows deleted, remaining files removed); if none was moved it rolls back with nothing to undo and the VM is not marked `recovering`.
- Tests: `vm-storage/tests/vm_delete.rs` (7), `media_registry.rs` (+2 for the checked and three-step import).

### Display ticket and proxy (`lightvm-service/src/display.rs`)

- Ticket: 32 random bytes from the IPC credential generator, 64 lowercase hexadecimal digits; compared in constant time (`Credential::ct_eq`); distinct from QMP (the QEMU `vnc.sock` path is never sent) and from the IPC credential.
- Listener: AF_UNIX `d.sock` in a fresh per-proxy directory `<display root>\<12 hex>\`; the display root (`<private root>\d`, ServiceConfig `display_dir`) gets the explicit user-only ACL at the first ticket (`qemu_backend::prepare::restrict_to_current_user`, same procedure as the QMP directory; `icacls` shows one ACE `(OI)(CI)(F)` for the user). The QEMU private directory of each run now also gets this ACL (`prepare_run_dirs`).
- Protocol: the client writes the token and `\n` within 5 s, then speaks RFB; anything else (wrong token, uppercase, short line, missing `\n`, timeout) closes the connection without contacting QEMU. At most 4 unauthenticated connections at a time.
- **One viewer per run** ("last wins": a new authenticated connection closes the previous one); **one valid ticket per VM** (a new `open_console` revokes the previous ticket).
- Revocation: any VM state other than `running` (stop, crash, reconciliation), Emergency Stop (priority path, before the termination), revocation **or close** of the UI connection that obtained the ticket, service shutdown. The listener and its directory are removed and live connections are closed (half-close on both ends). When the console is lost with its connection, the keys and buttons still pressed on the run are released (`QemuRun::release_all_input`, ROADMAP 6.4).
- `rfb_client::Transport::Ticket` (already committed) is the client side; the RFB client stays read-only.

### Input from the console

- The core accepts a pointer batch only on the current geometry. When a pointer batch carries a geometry different from the one known to the core, the engine re-reads the display geometry once from QEMU (`QemuRun::refresh_geometry`: `screendump` of which only the PPM header is parsed) and then the core decides: same geometry → accepted (a console can send input without a prior screenshot and after a guest resolution change); different → `state_conflict` with `reason: stale_geometry` (stale coordinates still rejected). Keyboard-only batches do not depend on the geometry.
- Latency: every batch went through 3 synced audit lines and 3 synced SQLite transactions (`synchronous=FULL`). Now `vm_input` audit lines are written without `sync_data` (they reach the disk with the next synced line: any lifecycle, configuration or denial event, never dropped) and input operations are not stored in the catalog `operations` table (never retried, no recovery state, kept in memory by the core for idempotency; each batch stays in the audit). Lifecycle audit is unchanged (synced).

## Procedure

```text
set CARGO_TARGET_DIR=C:\Woarkareas\OpenVM\target\service
cargo test -p lightvm-service -p vm-core -p vm-storage -p qemu-backend -p lightvm-proto -p lightvm-mcp -p rfb-client
cargo clippy -p lightvm-service -p vm-core -p vm-storage -p qemu-backend -p lightvm-proto -p lightvm-mcp -p rfb-client --all-targets -- -D warnings
cargo clippy -p lightvm-service --no-default-features --all-targets -- -D warnings
set LIGHTVM_E2E_QEMU_DIR=C:\Woarkareas\OpenVM\lab\qemu\qemu-11.1.0
cargo test --release -p lightvm-service --test f2 -- --ignored --nocapture e2e_real_qemu_import_console_input_stop_delete
```

The real test (`crates/lightvm-service/tests/f2.rs`, `#[ignore]`, `LIGHTVM_E2E_QEMU_DIR`; optional `LIGHTVM_E2E_ISO`, `LIGHTVM_E2E_BOOT_S` default 150) runs the service in process with the SQLite catalog, the file audit and the real QEMU backend, and through IPC: imports the ISO as installation media (while another connection calls `list_vms` every 250 ms), creates the VM, starts it, opens the console, completes the RFB handshake with `rfb_client::Connection::connect(Transport::Ticket)` and reads framebuffer updates in a thread, checks that a wrong token is refused, sends Enter on the boot menu, measures 100 keyboard batches and 100 pointer batches paced at 50 per second, checks the stale-geometry refusal, waits for the live system and sends a pointer batch on the new geometry and a screenshot, stops gracefully (ACPI 60 s → `quit` 10 s → kill 10 s), checks that the RFB session ends and the ticket is refused, starts a second run with a new ticket, resets it (console stays open), stops it by force, deletes the VM and checks rows and files, then removes the test catalog (6 GiB copy).

Lab adaptation (declared): the host had 8–9 GiB free while the core keeps a 16 GiB storage reserve, so `create_vm` would be refused (`resource_limit`, `disk_gib` limit 0). The test lowers the reserve to 0 for its own core (`free space 9 GiB below reserve 16 + 25 GiB: reserve set to 0 for this test`); the qcow2 disk is sparse and the live session does not write to it (`disk_used_bytes` 1 279 407 before deletion).

## Results

### Headless (fake backend, real core; SQLite catalog where stated)

Totals for the seven crates: **318 tests passed, 0 failed, 14 ignored** (real-QEMU and lab tests); clippy `-D warnings` clean, also without the `supervisor` feature (UI side).

| Item | Outcome | Evidence |
| --- | --- | --- |
| Pairing role refused on `list_media`, `import_media`, `delete_vm`, `open_console`, no effect | verified | `f2.rs::f2_requests_are_refused_for_the_pairing_role` |
| Invalid import paths refused (relative, UNC, `\\?\`, `\\.\PhysicalDrive0`, `//?/`, ADS, `NUL.iso`, `..`, `.txt`, directory, missing, no signature, empty, junction) with nothing imported | verified | `f2.rs::import_list_create_and_delete_with_the_sqlite_catalog`; `media/` contained only the accepted copy (since ADR-0029 it stays empty: the file is used in place) |
| Import → `list_media` → `create_vm` with the medium → start verifies the managed copy (real inspector) | verified | same test (`vm_start:f2-vm:media=1`); copy read-only |
| `disk_used_bytes` = VM files + run directories | verified | same test (4000 bytes written, 4000 reported) |
| `delete_vm` refused while running and with a stale revision; then files, directories and rows removed; VM absent after a restart; medium kept | verified | same test; audit `vm_delete` denied/succeeded, `media_import` succeeded/failed, no host path in the audit |
| Core: deletion refused for agents, while running, with queued operations (`vm_busy`), stale revision; catalog failure audited and VM kept | verified | `vm-core/tests/f2_catalog.rs` (4 tests) |
| Storage: deletion with baseline/overlay/vars/run files, unregistered files, alias directory; refusal with running VM, open run, junction in the tree, directories outside `vms/`/`runs/`; crash after a move rolls forward, before any move rolls back | verified | `vm-storage/tests/vm_delete.rs` (7 tests) |
| Ticket refused when the VM is stopped; ticket for the current run with geometry; QEMU socket path never exposed | verified | `f2.rs::console_ticket_is_checked_and_revoked_with_the_run` |
| Wrong token, uppercase token, short line, missing `\n`: closed without RFB and without contacting the display | verified | same test (fake display accepted no connection) |
| Handshake timeout | verified | same test: closed after 4–9 s (5 s configured) |
| Right token: RFB greeting from the display, transparent stream; second viewer closes the first; new ticket revokes the old one | verified | same test |
| Revocation on stop, on Emergency Stop, on revocation and on close of the owning UI connection; pressed keys released when the console is lost | verified | `console_ticket_is_checked_and_revoked_with_the_run`, `console_ticket_dies_with_the_ui_connection` |
| Input without a prior screenshot accepted; stale geometry refused (`stale_geometry`); keyboard-only independent of the geometry; wrong run refused; screenshot, reset, stop and forced stop reach distinct backend calls | verified | `f2.rs::input_from_the_console_without_a_screenshot_and_lifecycle_actions` |

### Real QEMU (run 6, `e2e-run6.log`; run 5 identical in outcome)

| Item | Outcome | Evidence |
| --- | --- | --- |
| Import of the Ubuntu ISO as installation media | verified | `import: 6482409472 bytes in 59.7015946s (104 MiB/s), sha256 601e30fb…1bda1f` (run 5: 22.7 s; run 2: 50.7 s) |
| Supervisor responsive during the copy | verified | `228 list_vms during the copy, max 0.4 ms` |
| `create_vm` with the imported medium, start with media verification | verified | `start (media verification included): 3.3942137s` (SHA-256 of the 6 GiB copy recomputed at start) |
| `open_console` and RFB handshake through the ticket proxy | verified | `open_console: 50.0688ms, geometry 640x480, socket %TEMP%\lvd…\8060c3ac4331\d.sock (token not shown)`; `RfbSeen { connected: true, init: (640, 480), updates: 1 … }` |
| Wrong token refused on the real proxy | verified | `ticket_refused` assertion passed |
| Framebuffer updates and guest resolution change seen by the client | verified | after the boot wait `updates: 263, resizes: 1, size: (1280, 800), non_black_permille: 999` (live desktop) |
| Keyboard batch IPC → core → QMP round trip, 100 samples at 50/s | verified | `p50 0.69 ms, p95 0.83 ms, max 1.24 ms` (run 5: 0.68 / 0.89 / 1.18 ms) |
| Pointer batch round trip on 1280×800 without a screenshot, 100 samples | verified | `p50 0.58 ms, p95 0.71 ms, max 10.05 ms` (the maximum includes the one-time geometry refresh) |
| Same measurement before the audit/catalog change (run 2) | failed (then fixed) | keyboard `p50 28.86 ms, p95 114.61 ms, max 207.42 ms`; pointer `p50 24.55 ms, p95 79.12 ms, max 355.01 ms` |
| Stale geometry refused on the real VM | verified | `state_conflict`, `reason: stale_geometry` |
| Pointer on the new geometry after the resize, screenshot | verified | operations `succeeded` |
| **Graceful stop** of the live desktop | **failed (ACPI)**, stop completed by escalation | `graceful stop: 61.1579296s (ACPI wait expired, then quit)`: the GNOME live session did not power off within 60 s (as in run 5 after a reset); the installed guest of `F1-03` powered off on ACPI in 3.55 s |
| Console revoked at the end of the run | verified | `ended: Some("connection closed")`, old ticket refused |
| Second run: new ticket, explicit reset with the console open | verified | `reset: 339.5706ms`; `RFB after the reset: updates: 7 … ended: None`; same `run_id` |
| Forced stop distinct from the graceful stop | verified | `forced stop: 71.9914ms`; ticket of the second run refused |
| Deletion with files and run directories | verified | `disk_used_bytes before deletion: Some(1279407)`; `VM deleted: 7 files (1 MiB), 3 directories`; `delete_vm: 45.7807ms`; `vms/<id>` and both `runs/<id>` gone |
| Audit | verified | `638 lines, vm_input 607, media_import 1, console_open 2, vm_delete 1`; the token does not appear in the audit file |
| No QEMU process left | verified | no `qemu-system-x86_64` process after the runs (checked with `Get-Process`) |

## Limits

- **Free disk space of the lab host.** The system SSD had 8–12 GiB free (other agents' build directories); the core's storage reserve is 16 GiB, so on this host `create_vm` is refused (`disk_gib` limit 0) for the product as configured: the real test lowered the reserve for itself. For the same reason the F1 test `core_path::e2e_real_qemu_create_start_status_stop` **was not executed** (it fails at `create_vm` with `resource_limit` before reaching QEMU, unrelated to F2).
- **Graceful stop occupies the supervisor thread** for up to the ACPI wait plus `quit` and kill (60 + 5 + 10 s with the service default): requests of every VM, including a forced stop of the same VM, wait behind it; Emergency Stop does not (priority path). During `stopping` the core refuses input, so a guest dialog that asks for confirmation (GNOME live session) cannot be answered from the console: the stop ends with `quit`.
- One real run sample per outcome (runs 5 and 6 complete; runs 1–4 failed for test or environment reasons, all reported in the logs: reserve, Stop over the 10 s test timeout, full disk, rate limit at back-to-back input).
- Input is subject to the per-connection rate limit (100 requests/s, burst 50): back-to-back batches without pacing hit `resource_limit` (run 4). The console must send at most one batch per frame.
- The per-run UEFI variables are a fresh copy of the template at every start (`runs/<run_id>/vars.fd`, F1 adapter): boot entries written by an installer are not kept between runs; boot from an installed disk relies on the removable-media fallback path. Not verified here (F2 exit criterion, installation through the UI).
- The proxy uses blocking threads (F0-04): a client that never closes its end after a revocation keeps one thread blocked until it does, without receiving anything.
- Second Windows user against the display directory: not executed (no second account, as `F1-11`).
- `vm_input` audit lines are not synced individually: a power loss may lose the last input lines (never lifecycle lines).

## Required actions

- Decide the storage reserve policy for small hosts (16 GiB fixed today) or document the free-space requirement in the user guide before the F2 exit test on this host.
- Optional: run graceful stops off the supervisor thread (the core already has `begin_dispatch`/`finish_dispatch`), so a forced stop can overtake a pending graceful stop and other VMs stay responsive.
- Verify installation to disk and boot from the disk through the service (fresh UEFI variables per run), F2 exit criterion.

## Proposed decisions

- ADR-0013, consequences (to be recorded by the coordinator): the per-run display credential is implemented as a supervisor proxy with a 32-byte ticket on an AF_UNIX socket in a user-only directory, distinct from QMP and IPC, revoked with the run, on Emergency Stop and with the UI connection; one viewer per run; evidence in this file. The obligation "per-run, distinct, revocable credential" can move to **satisfied on Windows** (same-user processes that do not hold the ticket get no framebuffer; a second user not tested).
- ADR-0008: path rules of `import_media` as listed above (UNC and device paths refused, reparse points refused, `.iso` only, signature checked on the copied bytes).

## Update of 2026-10-10 (later the same day): persistent firmware state per VM, graceful stop in the background

Section appended at the end (rule 8 of `docs/verification/README.md`). Same host, QEMU and ISO; the free space was raised to about 65 GiB by the coordinator, so the real test ran **without** lowering the storage reserve (`free_storage_gib: 65, reserved_storage_gib: 16`).

### Changes

1. **Persistent UEFI variables per VM** (ROADMAP 4, "firmware configured and its persistent state managed"): `vms/<vm_id>/firmware/primary.vars.fd` (`CatalogLayout::primary_vars`) is created from the template of the QEMU distribution at the first start (copy to a temporary file next to it, `sync_all`, publication with a hard link that never replaces an existing file) and reused at every later start (`adapters::backend::ensure_vm_vars`). The per-run copy `runs/<run_id>/vars.fd` no longer exists. The file is protected during the run by the storage guard (it is the `firmware_vars` of `QemuRun::start`, in the no-delete set like the disk), counted in `disk_used_bytes` and deleted with the VM (`vms/<vm_id>/`). Like the primary disk it is produced by the backend, not through the catalog journal (F3 gap, unchanged).
2. **Graceful stop in the background**: the engine forwards `stop` with `BackendPort::begin_shutdown`; the QEMU backend runs `QemuRun::shutdown` (ACPI → wait → `quit` → termination) as a task on its own runtime and the reply to `submit_vm` comes at once with the operation `forwarded` and the VM `stopping`. The outcome is collected by `finished_shutdowns` at every request and at the supervisor's idle tick (1 s); only then the core closes the operation and the run (`stopped`) and the `vm_state` event is published. A `force_stop` submitted meanwhile is forwarded immediately by the core and wins; the superseded graceful stop is closed as `state_conflict` ("superseded by a forced stop"); a late outcome changes nothing. If the escalation fails (process not terminated) the VM stays `stopping` and a forced stop resolves it. Controlled shutdown of the service and the parent-loss policies submit the stops of all VMs (they now run in parallel) and wait for them (each bounded by its escalation, safety bound 300 s); the per-VM shutdown budget of the server is unchanged (now an over-estimate). The console ticket is revoked when the VM has no process any more (`stopped`/`failed`/`recovering`), so the console stays open while the guest shuts down. IPC contract: documentation of `submit_vm` updated (schema `ipc_client_message.json` regenerated, no structural change); a backend without `begin_shutdown` (fakes by default) keeps the blocking behaviour.

### Results

| Item | Outcome | Evidence |
| --- | --- | --- |
| Variables created once and reused, never overwritten by the template; a non-regular file in their place refused | verified | `adapters::backend::tests::vm_vars_are_created_once_and_then_reused` |
| Real: variables of the VM written by the firmware during run 1 and reused by run 2, no per-run copy | verified | `UEFI variables of the VM after run 1: sha256 39048a8f20a1e09e… (template 5d2ac383371b4083…), per-run copy present: false`; creation time of the file unchanged at the start of run 2; `runs/<run2>/vars.fd` absent |
| Fake: `stop` replies at once (`forwarded`, VM `stopping`), console open while stopping, other requests served, second `stop` refused, `force_stop` executed at once and wins, late outcome ignored, exit observed in the background → `stopped` event without requests, failed escalation → `stopping` then forced stop, service shutdown waits for a stop in progress without killing | verified | `f2.rs::graceful_stop_runs_in_the_background_and_a_forced_stop_wins` |
| Real: graceful stop of the live desktop does not hold the supervisor | verified | `graceful stop: reply 9.5792ms (forwarded), stopped after 60.8279717s (ACPI wait expired, then quit); 121 vm_status during the stop, max 9.8 ms; console open while stopping: true`; backend `graceful stop: phase Quit, cause QmpQuit, 60.6096101s` |
| Real: forced stop during a graceful stop of the same VM | verified | `forced stop during the graceful stop: 75.6764ms, state Stopped` (graceful stop forwarded 3 s earlier) |
| Real: rest of the end-to-end (import, console ticket, input latency, reset, deletion) | verified | run 7 (`e2e-run7.log`): import 20.9 s, keyboard p50 0.64 / p95 0.76 / max 1.28 ms, pointer p50 0.57 / p95 0.74 / max 11.26 ms, reset 14 ms, `VM deleted: 6 files (0 MiB), 4 directories` (the `firmware/` directory included) |
| F1 real tests through the service with the background stop (`core_path::e2e_real_qemu_create_start_status_stop`, `e2e_real_qemu_cycles_through_the_service` 20 cycles, `e2e_real_qemu_vm_settings`) | verified | 3 passed (`e2e-core2.log`): 20 cycles, start median 192.4 ms, graceful stop (1 s ACPI → `quit`) about 1.13 s, forced stop about 49 ms, Emergency Stop 43.5 ms, leftover processes 0 in every cycle; the tests now accept a `forwarded` stop and wait for `stopped` |
| Headless totals (seven crates) | verified | 320 passed, 0 failed, 14 ignored; clippy `-D warnings` clean |

### Limits (update)

- The limit "graceful stop occupies the supervisor thread" of the first section no longer applies. The ACPI stop of the GNOME live session still does not complete within 60 s (escalation to `quit`); during `stopping` the core still refuses input, so a guest confirmation dialog cannot be answered from the console.
- The end of a background stop is observed with a delay of up to 1 s (supervisor idle tick) when no requests arrive.
- Existing VMs keep no previous variables: their first start after this change creates `primary.vars.fd` from the template (the old `runs/<run_id>/vars.fd` copies are left in place until the VM is deleted).
- Installation to disk and boot from the disk through the service remain to be verified (F2 exit criterion); the persistent variables remove the dependency on the removable-media fallback path noted in the first section.

## Update of 2026-10-10 (evening): input while stopping, F2 exit criterion path through the service

Section appended at the end (rule 8). Same host, QEMU and ISO; about 65 GiB free, storage reserve not lowered.

### Changes

1. **User input while `stopping`**: `vm_core::input_allowed_for(caller, state)` allows input in `running` (base rule `action_allowed_in`, unchanged) and, **for the user only**, in `stopping` (a GNOME/Ubuntu guest shows a power-off confirmation after a graceful stop request). Agent sessions keep the base rule (F4). The forwarded graceful stop does not make the VM busy for that input, and the core forwards the input alongside it. The run and geometry checks are unchanged. States without a process still refuse input. In `qemu-backend`, `QemuRun::send_input`, `refresh_geometry` and `release_all_input` accept a process that is `running` or `stopping` (`ensure_alive`).
2. **Real test of the F2 exit criterion path** `f2.rs::e2e_real_qemu_f2_exit_install_reboot_reopen` (`#[ignore]`, `LIGHTVM_E2E_QEMU_DIR`; optional `LIGHTVM_E2E_ISO`, `LIGHTVM_E2E_CIDATA`, `LIGHTVM_E2E_CPU` (default `Skylake-Client-v4`, as F1-17), `LIGHTVM_E2E_INSTALL_MIN` (45), `LIGHTVM_E2E_EVIDENCE_DIR`). The test never reads `lab/vms/f1-17/`. Everything goes through IPC:
   - import of the Ubuntu ISO (install media) and of `lab/media/f1-17-cidata.iso` (data media, autoinstall of F1-17);
   - VM with 4 vCPUs, 6144 MiB and a 25 GiB disk; start and console ticket;
   - at 140 s, a click on "Install" through the service's input path (`submit_vm` input; same position as F1-17, computed on the RFB geometry);
   - unattended installation until the guest powers off;
   - `update_vm` removing both media, as the UI does;
   - start from the disk, with the RFB framebuffer checked (1280×800, mostly lit, stable 10 s), then graceful stop;
   - service shutdown, then **"reopen LightVM"**: a new service on the same catalog (new probe, new SQLite open), VM listed, start from the disk, framebuffer checked, graceful stop, deletion.

### Procedure

```text
set CARGO_TARGET_DIR=C:\Woarkareas\OpenVM\target\service
set LIGHTVM_E2E_QEMU_DIR=C:\Woarkareas\OpenVM\lab\qemu\qemu-11.1.0
set LIGHTVM_E2E_EVIDENCE_DIR=<scratch directory>
cargo test --release -p lightvm-service --test f2 -- --ignored --nocapture e2e_real_qemu_f2_exit_install_reboot_reopen
cargo test --release -p lightvm-service --test f2 -- --ignored --nocapture e2e_real_qemu_import_console_input_stop_delete
```

### Results

| Item | Outcome | Evidence |
| --- | --- | --- |
| Rule `input_allowed_for` (user: `running`, `stopping`; agent: `running`; other states refused), input forwarded alongside a forwarded stop, stale run refused, agent refused while stopping, refused after the end of the run | verified | `vm-core/tests/f2_catalog.rs::user_input_is_accepted_while_stopping_agents_and_stopped_vms_are_refused` |
| Fake: input during a background graceful stop | verified | `f2.rs::graceful_stop_runs_in_the_background_and_a_forced_stop_wins` |
| Real: input during the graceful stop of the live desktop | verified | `e2e-run8.log`: `input while stopping: succeeded in 932.9µs`; then `stopped after 61.3441147s (ACPI wait expired, then quit)`; forced stop during a graceful stop 89.8 ms; keyboard p50 0.68 / p95 0.79 ms |
| Imports (install and data media) | verified | `import InstallIso: 6482409472 bytes in 15.5375432s, sha256 601e30fb…1bda1f`; `import DataIso: 65536 bytes in 9.9062ms, sha256 85145f96…a476f1` (equal to F1-17) |
| Autoinstall summary shown, click on "Install" through the service input path | verified | `click on Install (1035,716) on 1280x800 at 143.6s`; framebuffer of the "Ready to install / Review your choices" summary saved (contains the password hash of the test user: kept out of the repository) |
| **Installation to disk through the service, guest powering itself off** | verified | `installation finished, guest powered off after 423.895022s; disk 6882983936 bytes; UEFI variables d9599600d65ba486 (template 5d2ac383371b4083)`. The boot entry was written into the per-VM variables |
| Media removed with `update_vm` (VM off) | verified | `VmUpdated`; `install media None` after reopening; audit `vm_configure` succeeded 1 |
| **Boot from the disk** | verified | `installed system on screen after 25.035645s`: GDM login screen of the installed system ("LightVM prova"); framebuffer PNG SHA-256 `23d217ad20929656…` |
| Graceful (ACPI) stop of the installed system | verified | `graceful stop: phase Powerdown, cause GuestShutdown, 3.5804986s` (reply forwarded, end observed in the background) |
| **Reopen: new service on the same catalog finds the VM and boots it from the disk** | verified | `after reopening: f2-exit-ubuntu Stopped, install media None, disk_used_bytes Some(6896108936)`; `installed system on screen after 20.027969s` (GDM login screen, PNG SHA-256 `90263f3c485e0586…`); graceful stop `Powerdown, GuestShutdown, 3.5152441s` |
| Deletion of the installed VM | verified | `VM deleted: 8 files (6577 MiB), 5 directories`; audit 38 lines, `vm_start` succeeded 3, `vm_stop` succeeded 2 |
| Headless totals (seven crates) | verified | 321 passed, 0 failed, 15 ignored; clippy `-D warnings` clean (also without the `supervisor` feature) |

Raw output: `e2e-exit1.log` and `e2e-run8.log` in the session scratchpad. The framebuffer PNGs are `f2-exit-1…4` in the evidence directory. They are not copied to the repository: the summary screen shows a password hash.

### Limits

- The CPU model of this test is `Skylake-Client-v4` (`LIGHTVM_E2E_CPU`), as in F1-17. The service default is still `qemu64` (ADR-0011). The installation with `qemu64` was not executed. Whether the product default changes is a decision for ADR-0011.
- The click on "Install" is at a fixed position and time (140 s), as in F1-17. A slower host could need a later click. The desktop UI (agent "desktop") is not part of this test: the user's path through the wizard and the console is `F2-02`.
- One sample of the full path.
- The installed system was checked only through its login screen; nobody logged in. The test credentials of F1-17 were not used or read.

## Update of 2026-10-10 (night): review findings on the service (agent "service")

Fifteen findings of the F2 review on `crates/**`, each fixed with a regression test (fake backend or unit) where the behaviour can be reproduced without QEMU. Nothing in `apps/desktop` or `packaging` was touched.

### Changes

| # | Finding | Fix | Regression test |
| --- | --- | --- | --- |
| 1 | `validate_import_source` sliced `raw[2..]` (panic on a multi-byte character, e.g. `€:\a.iso`) on the supervisor thread: a panic killed every VM | Byte-level checks only (`validate_import_path`). **Panic containment**: every command and every observation round of the supervisor loop runs under `catch_unwind`; the panic is logged, the waiting request gets `infrastructure_error`, the thread keeps the engine, the host, the runs and their Job Objects | `adapters::catalog::tests::import_path_validation_never_panics` (fixed list plus 20 000 pseudo-random strings over separators, colons, dots and multi-byte characters); `supervisor::tests::a_panic_while_serving_a_request_does_not_stop_the_supervisor` |
| 2 | The display proxy forwarded raw client bytes: a ticket holder could inject KeyEvent/PointerEvent/ClientCutText | `display::ClientFilter` parses client → server RFB: exactly `RFB 003.008\n`, security None, ClientInit 0/1, then only SetPixelFormat (0), SetEncodings (2, at most 64) and FramebufferUpdateRequest (3, flag 0/1), released as whole messages; KeyEvent, PointerEvent, ClientCutText, QEMU extensions or anything else close the connection without forwarding a byte of it | `display::tests::filter_*`, `key_event_closes_the_connection_and_is_never_forwarded` (fake RFB upstream records every byte: only handshake and FBUR arrive), `rfb_client_session_works_through_the_filter` (real `rfb-client` through the ticket), `f2.rs::console_ticket_is_checked_and_revoked_with_the_run` (KeyEvent through the service closes the viewer) |
| 3 | UNC check textual only; mapped/`subst` drives and intermediate links passed; file checks blocked the supervisor thread | The supervisor thread only checks the text and records the journal intent (`vm_storage::Catalog::reserve_media_import`); the import worker thread classifies the path, opens the source (share read, `FILE_FLAG_OPEN_REPARSE_POINT`), checks type, size, ISO/UDF signature, free space and **location on the handle**: `GetFinalPathNameByHandleW` + `GetVolumePathNameW` + `GetDriveTypeW` (new safe wrapper `qemu_backend::prepare::file_location`). Refused: UNC final path or volume not fixed/removable/CD-ROM (`permission_denied`); **reparse traversal rule**: the final path must equal the requested path (case-insensitive, `/` = `\`), any difference (intermediate symlink, junction, mount point, `subst`/mapped drive, 8.3 short name) is refused with "choose the file in its real location". Reparse points on the last component stay refused | `adapters::catalog::tests::import_checks_run_on_the_handle` (junction in an intermediate directory refused, same file through its real path accepted, case-insensitive); `vm-storage/tests/media_registry.rs::reserved_import_opens_the_source_only_on_the_worker`; `qemu-backend` `prepare::tests::location_of_a_local_file_and_pid_liveness`; `f2.rs::import_list_create_and_delete_with_the_sqlite_catalog` (all previous refusals with the same codes) |
| 4 | A graceful stop whose escalation timed out left the VM in `stopping` and its later exit was filtered as Emergency Stop | Backend: such a run stays **awaiting exit** (not released) and its exit is reported whatever the cause (the escalation's own termination included). Core (documented in `finish_dispatch`): `(stop, Err)` keeps the run current and the VM `stopping` (console, user input, forced stop and Emergency Stop still possible); the observed exit closes the run as `stopped` | `vm-core/tests/f2_catalog.rs::graceful_stop_timeout_stays_stopping_until_the_exit_is_observed`; `supervisor::tests::graceful_stop_timeout_waits_for_the_exit`; `adapters::backend::tests::stop_and_exit_routing` |
| 5 | Requests queued just before the connection closed still ran (dropped watch sender not treated as revoked) | `supervisor::is_revoked` = flag raised **or** sender gone; the server raises the flag before removing the connection entry (normal close and revocation) | `supervisor::tests::requests_of_a_closed_connection_are_not_executed` |
| 6 | Ticket revocation relied on state diffs: `running → stopped → running` in one command kept the old ticket | The engine compares `(state, run)`; at every publication the supervisor calls `revoke_other_runs(vm, current_run)` for every VM and revokes tickets of VMs that no longer exist | `supervisor::tests::a_run_replaced_within_one_command_revokes_the_console_ticket` |
| 7 | An Emergency Stop during a pending graceful stop was recorded as an ordinary stop | The priority handler marks `(vm, run)` **before** terminating; marked runs are never reported by `finished_shutdowns` or `drain_observations` and are left to `after_emergency_stop` (`force_stop` confirms); the escalation kill (not marked) is reported normally | `supervisor::tests::emergency_stop_during_a_graceful_stop_is_recorded_as_emergency_stop`; `adapters::backend::tests::stop_and_exit_routing` |
| 8 | Revoked proxy connections leaked threads (`shutdown(Write)` only) | `shutdown(Both)` on both ends **and**, because on Windows a shutdown does not unblock a `recv` in progress on AF_UNIX (verified: with `shutdown(Both)` alone one thread stayed), the authenticated sockets are non-blocking and the pumps poll a per-connection stop flag (pauses ≤ 4 ms when idle) | `display::tests::revocation_ends_both_threads_of_an_idle_client` (idle client keeping its end open: 0 threads after revocation and after replacement) |
| 9 | Token in `Debug` | Manual `Debug` redacting the token on `lightvm_proto::ipc::ConsoleTicket` and `rfb_client::Transport` | `ipc::tests::console_ticket_debug_redacts_the_token`; `transport::tests::ticket_debug_redacts_the_token` |
| 10 | `console_open` succeeded audited before the proxy existed; revocations not audited | `Core::open_console` audits only refusals; `Core::record_console_open` after the proxy (succeeded or failed with the code); new additive `AuditAction::ConsoleRevoke` with the cause in `termination` (`vm_terminated` run end or replacement by another run, `ui_closed`, `revoked`, `emergency_stop`, `completed` for a new ticket and for shutdown); schemas regenerated (`audit_event.json`; `ipc_server_message.json` only for the doc of `ConsoleTicket`) | `vm-core/tests/f2_catalog.rs::console_ticket_requires_a_running_vm_and_is_audited`; `supervisor::tests` (revoke audited with `vm_terminated`; open audited once); real e2e (counts below) |
| 11 | A failed start leaked the private socket directory | `PrivateDirGuard` created before `prepare`, disarmed only when the run is registered: every error path (preparation, firmware, media, disk, memory, launch) removes it | `adapters::backend::tests::failed_start_steps_remove_the_private_dir` (guard semantics; the full `start` needs QEMU) |
| 12 | `OnceLock` cached an error forever and never recreated a deleted display root | The root is (re)created with its ACL whenever missing; failures are not cached. At service start (instance lock held) `DisplayRegistry::remove_stale` removes per-proxy directories (12 hex digits) left by a previous service; `remove_stale_fallback_roots` removes `%TEMP%\lightvm-p-<pid>` of dead processes (PID not running; access denied counts as running) | `display::tests::root_is_recreated_and_stale_directories_are_removed`, `dead_fallback_roots_are_removed` |
| 13 | Observation starved by steady traffic | Fixed deadline: observation at least every `OBSERVE_INTERVAL` (1 s) whatever the traffic | `supervisor::tests::exits_are_observed_within_the_deadline_under_steady_traffic` (Ping every 20 ms, exit published within 2.2 s) |
| 14 | `refresh_geometry` (a `screendump`) ran before authorization | Refresh only if `Core::is_authorized` (side-effect-free) and input is allowed in the state, and at most once per 500 ms per run | `supervisor::tests::geometry_refresh_is_authorized_and_debounced` |
| 15 | Imports not cancelled by Emergency Stop or revocation | `import_media` is a mutation (discarded if queued before an Emergency Stop); the finish step receives the connection's revocation watch and the epoch: if revoked/closed or the epoch changed, the copy is discarded (`Catalog::abort_media_import`: intent aborted, temporary removed) and the import audited as failed | `supervisor::tests::imports_are_cancelled_by_revocation_and_emergency_stop`; `media_registry.rs::reserved_import_opens_the_source_only_on_the_worker` |

### Procedure

```text
set CARGO_TARGET_DIR=C:\Woarkareas\OpenVM\target\fix-service
cargo fmt -p lightvm-service -p vm-core -p vm-storage -p lightvm-proto -p rfb-client -p qemu-backend
set LIGHTVM_UPDATE_SCHEMAS=1 & cargo test -p lightvm-proto --test schema_snapshot
cargo clippy --all-targets -p lightvm-service -p vm-core -p vm-storage -p lightvm-proto -p rfb-client -p qemu-backend -p lightvm-mcp -- -D warnings
cargo clippy --all-targets -p lightvm-service --no-default-features -- -D warnings
cargo test -p lightvm-service -p vm-core -p vm-storage -p lightvm-proto -p rfb-client -p qemu-backend -p lightvm-mcp
set LIGHTVM_E2E_QEMU_DIR=C:\Woarkareas\OpenVM\lab\qemu\qemu-11.1.0
cargo test --release -p lightvm-service --test f2 -- --ignored --nocapture e2e_real_qemu_import_console_input_stop_delete
```

### Results

| Item | Outcome | Evidence |
| --- | --- | --- |
| Headless totals (seven crates: lightvm-service, vm-core, vm-storage, lightvm-proto, rfb-client, qemu-backend, lightvm-mcp) | verified | 345 passed, 0 failed, 15 ignored (real-QEMU tests) |
| clippy `-D warnings`, `cargo fmt --check` | verified | clean, also `lightvm-service` without the `supervisor` feature; `lightvm-desktop` still compiles against the changed crates (`cargo check --all-targets`, not modified) |
| Schemas | verified | `audit_event.json` gains `console_revoke`; snapshot test passes |
| **Real e2e through the filtering proxy** (`e2e_real_qemu_import_console_input_stop_delete`, release, WHPX, Ubuntu 26.04.1 ISO) | verified | `e2e-fix1.log`: import `6482409472 bytes in 32.65s` (copy on the worker, handle checks included; 126 `list_vms` during the copy, max 0.7 ms); `open_console: 65.7 ms`; `rfb-client` through the ticket and the filter: `updates: 472, resizes: 1, size: (1280, 800), non_black_permille: 999` after the boot wait; keyboard IPC→QMP p50 1.44 / p95 5.03 ms, pointer p50 1.15 / p95 3.16 ms; input while stopping succeeded in 0.96 ms; graceful stop of the live desktop `phase Quit, cause QmpQuit, 60.43 s` (as before: ACPI wait expired), console open while stopping; reset 19.1 ms with RFB updates after it; forced stop during a graceful stop 79.8 ms; deletion 44.1 ms; audit `console_open 2, console_revoke 2`, token not in the audit; `test result: ok. 1 passed … finished in 283.68s` |

Raw output: `e2e-fix1.log` in the session scratchpad.

### Limits

- Finding 8: on Windows a `shutdown(Both)` alone did **not** end the thread blocked in `recv` on the client socket (measured: one thread left). The fix polls non-blocking sockets (pauses up to 4 ms when idle, ≤ 250 µs right after traffic): a little CPU per idle console and up to a few ms added to a client request that arrives after an idle period; the real e2e shows no visible effect on the console.
- Finding 3: the reparse traversal rule also refuses legitimate paths that the system resolves differently: 8.3 short names, `subst` drives, folders reached through a junction (for example a user who moved a library with a junction). The message asks to choose the file in its real location. Mapped network drives and UNC shares are refused by design. Non-Windows systems keep only the textual and handle-type checks (`file_location` is not implemented there).
- Finding 11: the regression test covers the guard on a failing step; the full `QemuVmBackend::start` error paths need QEMU and were not forced to fail in the real run.
- Findings 4 and 7 on real QEMU: the routing is unit-tested (`stop_and_exit_routing`) and the core/engine behaviour with the fake backend; an escalation timeout and an Emergency Stop during a real graceful stop were not reproduced with QEMU in this run.
- Finding 12: a stale fallback root is removed only when its PID is not running; a PID reused by another process keeps its directory until the next start after that process ends.
## Update of 2026-10-11: start checks off the supervisor thread

The approved media are re-hashed at every start (ADR-0008). Until now this ran on the supervisor thread inside `submit_vm(start)`: with the 6.4 GB ISO (about a minute on a cold cache) nothing else was served meanwhile (console input of the other VMs, stops, status, events). Since F3 the same happened for the baseline base disk and UEFI variables (~0.8 s/GiB).

### Changes

- **Three steps, like the media import and the baseline jobs** (`crates/lightvm-service/src/start.rs`):
  1. *begin* (supervisor thread, `CoreEngine::begin_start`): the core forwards the start (`starting`, run created); every file to verify is **opened** with its guard and the handle is kept: media copies through `vm_storage::MediaGuard` (`FILE_SHARE_READ` only, `FILE_FLAG_OPEN_REPARSE_POINT`, every path component checked against links, regular file, ISO 9660/UDF signature read on the handle), the current-state files through `Catalog::open_current_state` (the run guard of ADR-0025 without the hash). A writer already holding a file makes the open fail (`vm_busy`) and the start is refused, as before.
  2. *work* (thread `lightvm-start-check`, `StartJob::run`): SHA-256 read **through those same handles**, in 1 MiB blocks, cancellable between blocks, progress published for `VmSummary::start`.
  3. *finish* (supervisor thread, `Command::FinishStart` → `VmEngine::finish_start`): if the run is still the VM's current one and nothing cancelled it, the observed facts go to the core, which still makes the comparison with the registry (`vm_core::media::verify_media` through a precomputed inspector), then QEMU is launched. The media handles are handed to the backend (`BackendPort::set_start_media`) and kept with the catalog guard until the run is released.
- **Why the file verified is the file QEMU opens.** The handle is opened before the hash and closed only when the run is released. While it is open (no `FILE_SHARE_WRITE`, no `FILE_SHARE_DELETE`) the copy cannot be written, renamed or deleted, and no directory above it can be renamed (checked on this host: renaming `media\` or a parent fails with "access denied" while the file is open; test `start_media_guard_pins_the_verified_file`). QEMU opens the service-chosen managed path read-only (`file.read-only=on`), which is compatible with the guard; so the bytes hashed are the bytes QEMU reads. The previous inspector hashed by path and closed the file before QEMU opened it (TOCTOU window, a TODO since F1-04): now closed. Limits unchanged from ADR-0025: administrator, raw volume access, a memory mapping made before the guard, no share modes on Linux.
- **Cancellation**: forced stop of the VM (`run_queue` raises the job flag), Emergency Stop (`after_emergency_stop` raises every flag; the epoch check of the worker too), revocation or closing of the connection that sent the start, service shutdown (the shutdown waits, bounded, for the start workers like for the baseline jobs). Nothing is launched; the start operation fails (`state_conflict`, or `unauthenticated` for a revocation) and the VM is `stopped`. A copy whose hash differs still makes the start fail with `permission_denied` / `media_identity_mismatch` and the VM `failed`, as before.
- **Reply**: `submit_vm(start)` keeps its contract (the reply carries the final operation record, sent when QEMU runs or the start failed). The server queues it in order and awaits the reply in a task (`submit_start_deferred`), like `import_media`: the connection keeps serving other requests and events meanwhile; the late response is matched by `request_id`. A client that sends requests concurrently on one connection must match responses by `request_id` (`Client::request` returns the next non-event message).
- Engines used directly (tests; `offload_start_checks(false)` is the default) run the same job inline: same code path, synchronous. The supervisor loop turns offload on.

### Contract additions (additive, schema regenerated: `ipc_server_message.json`)

- `VmSummary.start: Option<StartProgress>` (omitted from JSON when absent) with `StartProgress { phase, verified_bytes, total_bytes }` and `StartPhase` = `verifying_media` | `verifying_baseline` | `launching`. Present while the VM is `starting` and its checks run. The UI receives the `vm_state(starting)` event as before and polls `vm_status` / `list_vms` for the progress ("Verifying media… 41 %"); no new event.

### Tests

- `supervisor::start_tests` (fakes, slow "hash" behind a gate): the supervisor serves `vm_status` / `list_vms` / ping during the verification with the sub-status (with the real loop thread, 20 status requests each answered in < 500 ms); the start reply waits; launch with the media handles registered before `vm_start`, handles dropped only at the stop; forced stop, Emergency Stop, revocation and shutdown cancel (no launch, VM `stopped`, handles dropped, a new start works afterwards); a copy tampered during the verification is refused (`permission_denied`, VM `failed`).
- `adapters::catalog::tests::start_media_guard_pins_the_verified_file` (real files): while the guard is held, write / rename / delete of the copy and rename of `media\` fail even with the read-only attribute removed; progress counts every byte; cancellation; a writer already present → `vm_busy`; a copy changed before the start → hash mismatch; not an ISO → `invalid_argument`; missing → `not_found`.
- `tests/ipc.rs::start_reply_waits_for_the_checks_while_the_connection_keeps_serving`: on one connection, a `vm_status` sent after the start is answered first (`starting`, `verifying_media`), then the start response.
- `vm-storage/tests/baseline.rs`: the split check (`open_current_state` + `RunGuard::verify_baseline`) detects the tampered base, is cancellable, reports progress.
- All crates: 380 tests passed, 0 failed (18 ignored real/long tests); `clippy -D warnings` clean on all crates' targets and on `lightvm-service --no-default-features`.

### Real run (QEMU 11.1.0, WHPX, release build)

`cargo test --release -p lightvm-service --test f2 e2e_real_start_checks -- --ignored --nocapture` with `LIGHTVM_E2E_QEMU_DIR=lab/qemu/qemu-11.1.0` and the 6 482 409 472-byte Ubuntu 26.04.1 ISO:

| Measure | Value |
| --- | --- |
| Import (copy + hash) | 24.4 s |
| Start reply (hash of the managed copy through the guard + QEMU launch) | 4.20 s (≈1 471 MiB/s: the copy was still in the OS file cache after the import) |
| Sub-status seen | `starting` / `verifying_media` from 0.1 s (155 MB hashed) to 3.86 s (6.37 of 6.48 GB) |
| `vm_status` during the start (other connection, 37 samples, every ~100 ms) | p50 0.47 ms, p95 0.52 ms, max 232.7 ms |
| `list_vms` during the start (37 samples) | p50 0.26 ms, p95 0.34 ms, max 0.85 ms |
| Console ticket + first framebuffer update after the start | 186 ms |
| Managed copy during the run | rename refused, open for writing refused (read-only attribute removed first) |

The single 232 ms sample is the QEMU launch itself (`QemuRun::start`, still on the supervisor thread after the checks), not the hash. The hash duration does not affect these latencies (the supervisor does not take part in it); a cold-cache run (about a minute for this ISO) only lengthens the `verifying_media` phase.

### Not done / limits

- The QEMU launch (process start, QMP handshake, accelerator and NIC checks, `qemu-img` grow of the disk) still runs on the supervisor thread: ~0.2 s here.
- No cold-cache measurement in this run (the copy had just been written; flushing the file cache needs administrator rights).
- Hashes are not skipped for unchanged files: see the proposal below.

### Proposal (not implemented): skipping the hash of unchanged files

Idea: record at verification time the OS identity of the file (volume serial number, file index), size, last write time and read-only attribute, and skip the hash when they are unchanged, with a full hash on the first use after a supervisor start or every N starts. Not implemented, because it does not keep ADR-0008's guarantee ("hash re-verified at every start"):

1. the last write time is settable by any process of the same user (`SetFileTime`): a change made while the service is running followed by restoring the time stamp is invisible to the identity check (same file index, size, attributes);
2. raw volume writes and writes through a memory mapping made before the guard bypass the time stamp too (already declared limits of ADR-0025, but today the hash at the next start still catches the change; with the cache it would not);
3. the identity is not readable with stable `std` without `unsafe` (`vm-storage` and `lightvm-service` contain no `unsafe`); it would need `GetFileInformationByHandle` in `qemu-backend`'s FFI layer.

A variant that keeps a hard guarantee: keep the media guard open *between* runs (from the first verification until the service stops, `FILE_SHARE_READ` only), so the file cannot change while the service lives, and hash only when the guard is (re)acquired. This changes the rule from "hash at every start" to "hash at every acquisition of a continuous guard"; it needs an ADR-0008 / ADR-0025 decision (and makes the copy undeletable while the service runs).

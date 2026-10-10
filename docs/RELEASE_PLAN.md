# Release plan and backlog

Operational document required by section 17 of `docs/ROADMAP.md`: backlog, owners and execution order. **It neither widens nor narrows the frozen scope.** No dates and no durations: progress depends on the exit criteria, not on the calendar.

Sequential phases (section 10): F1 starts only after the F0 exit criterion. Milestone v0.1 at the end of F2, MVP v0.2 at the end of F6 (sections 4 and 10).

## F0 backlog

Type legend: **B** = blocking for exiting F0; **NB** = started in F0, can be completed within F1 without violating the exit criteria (section 10, F0).

| ID | Activity | Type | Step (sec. 15) | Owner | Evidence / output | ADR | Outcome |
| --- | --- | --- | --- | --- | --- | --- | --- |
| F0-B01 | Verify the Rust toolchain; real minimal eframe build on Windows | B | 1 | to be assigned | `F0-01-toolchain-eframe.md` | 0001, 0014 | completed with limits: `F0-01` (explicit MSVC environment) |
| F0-B02 | Minimal eframe build on Linux; rendering backend; behavior without graphics acceleration (RDP) | B | 1 | to be assigned | `F0-01-toolchain-eframe.md` or a later file | 0014 | partial: Linux build in WSL2 (`F0-07`), WARP verified (`F0-01`); RDP not run (limit L-1) |
| F0-B03 | Freeze scenario, threats, targets, dependencies: initial ADRs, initial matrix | B | 1 | to be assigned | `docs/adr/`, `docs/COMPATIBILITY.md` | all | completed |
| F0-B04 | Obtain and verify the Ubuntu LTS ISO (checksum, signature); pin the firmware | B | 1–2 | to be assigned | `F0-03-guest-media.md` | 0011 | completed: `F0-03`, firmware in ADR-0011 |
| F0-B05 | Install QEMU with verified provenance; pin binary and version | B | 2 | to be assigned | `F0-02-qemu-whpx-qmp.md` | 0010 | completed with deviation D-4 (expired signature): `F0-02` |
| F0-B06 | Guest boot on Windows/WHPX with a documented configuration; probe without user credentials in the guest | B | 2 | to be assigned | `F0-02-qemu-whpx-qmp.md` | 0007, 0010 | completed with deviation D-1 (`-cpu qemu64`): `F0-02` §9 |
| F0-B07 | QMP on a private channel; ordinary and forced shutdown | B | 2 | to be assigned | `F0-02-qemu-whpx-qmp.md` | 0012 | completed: `F0-02` §4, §7; ACPI shutdown not completed by the live system (D-7) |
| F0-B08 | Native console with RFB client on the protected channel, or screendump fallback; input; screenshots | B | 3 | to be assigned | `F0-04-console-rfb-input.md` | 0003, 0013 | completed with pending items P-4, P-5: `F0-04` |
| F0-B09 | Immutable baseline / overlay / restore cycle with persistent firmware; interruption tests | B | 4 | to be assigned | `F0-05-baseline-overlay-firmware.md` | 0005, 0011 | completed: `F0-05` |
| F0-B10 | Supervisor process model: QEMU as child, OS termination of children, UI crash, supervisor crash | B | 4 | to be assigned | `F0-06-process-supervisor.md` | 0002, 0009, 0015 | completed on Windows: `F0-06`; UI crash in `F1-09` (ADR-0015 B accepted on 2026-10-10) |
| F0-B11 | Boot on Linux/KVM if the lab is available, otherwise record "not run" | B | 5 | to be assigned | `F0-07-linux-kvm.md` | 0016 | lab unavailable: `F0-07`; outcome "verified on Windows only" confirmed on 2026-10-10 (P-1 closed, ADR-0016) |
| F0-B12 | OS limits for CPU, memory, storage; first measurement of resource usage | B | 5 | to be assigned | `F0-08-resource-limits-measurements.md` | 0017, 0018 | completed with limits: `F0-08` |
| F0-B13 | Threat model, MVP restrictions, initial support matrix, fixture definition | B | 1, 6 | to be assigned | `docs/SECURITY.md`, `docs/COMPATIBILITY.md`, ADR-0019 | 0004, 0019 | completed: fixture in ADR-0019 |
| F0-B14 | First version of `ARCHITECTURE.md` and `SECURITY.md`; ADRs for the critical decisions | B | 1, 6 | to be assigned | documents in `docs/` | all | completed |
| F0-B15 | Check the availability of code signing and independent review | B (decision to close in F0, sec. 17) | 1 | to be assigned | `docs/STATUS.md` | 0020 | closed on 2026-10-10: signing and review not available (P-6, ADR-0020 accepted) |
| F0-B16 | Evidence review; continuation decision; F1 backlog | B | 6 | to be assigned | `F0-09-evidence-review.md`, `docs/STATUS.md`, this file | — | completed: `F0-09` (proposal awaiting confirmation) |
| F0-N01 | Selection and testing of the MCP SDK and of the reference clients | NB | — | to be assigned | dedicated verification file | 0021 | partial: SDK chosen and tested (`F1-06`); real MCP clients deferred to F5 |
| F0-N02 | Inventory of dependency packaging and licenses | NB | — | to be assigned | dedicated verification file | 0023 | completed for inventory and `cargo deny` (`F1-07`); SBOM open until F6 |
| F0-N03 | Complete `COMPATIBILITY.md` and `TESTING.md`; choice of the UI test framework | NB | — | to be assigned | documents in `docs/`; verification file | 0022 | partial: egui_kittest tested (`F0-01`, `F0-07`); `COMPATIBILITY.md` and `TESTING.md` completed in commit `aa04ccd`; snapshots without evidence |
| F0-N04 | Implementation of the fixture in the guest (by F3, section 9) | NB | — | to be assigned | verification file in F1–F3 | 0019 | open (by F3) |

## Execution order

1. F0-B03, F0-B13 (draft), F0-B14 (draft), F0-B15: documentation freeze of step 1.
2. F0-B01, F0-B04, F0-B05 in parallel: they do not depend on each other.
3. F0-B06, then F0-B07: they require B04 and B05.
4. F0-B08: requires B07 (QMP for input and screendump) and B01 (minimal UI).
5. F0-B09 and F0-B10: require B06 and B07; B10 provides the termination path used by the interruption tests of B09.
6. F0-B02, F0-B11, F0-B12: B11 depends on the availability of the Linux host; B12 requires B10 (process tree under supervision).
7. F0-B16: requires all previous blocking activities with a recorded outcome (verified, failed, or not run with a reason).

NB activities may proceed at any time without blocking step 6, as long as they do not anticipate an extensive dashboard construction (section 15).

## Referenced exit criteria

- F0: exit criterion, stop or target-change decision, "Linux lab unavailable" rule (section 10, F0).
- F1–F6: exit criteria of section 10 for each phase.
- Public MVP: Definition of Done (section 11).

A completion statement without a mandatory exit criterion is not accepted (section 16, item 8).

## Closing F0

F0 closes when the pending items P-1, P-2 and P-4…P-7 of `docs/verification/F0-09-evidence-review.md` §3.1 have a recorded outcome. F1 starts after the F0 exit criterion (section 10); F1 activities already started do not count towards the F1 exit criterion before that closure.

## F1 backlog

Derived from section 10 F1 and from the actions required in the F0 files (review in `F0-09`). All owners are to be assigned. The activities do not widen the scope: each one translates an F1 roadmap activity or an open point that emerged from the tests.

| ID | Activity | Origin | Expected evidence | ADR |
| --- | --- | --- | --- | --- |
| F1-01 | Workspace, reproducible build with `--locked`, lint, SQLite migrations, IPC/MCP types and JSON Schema generated from a single source with stable public errors | sec. 10 F1; sec. 6.3 | tests and build in CI | 0002 |
| F1-02 | Windows/Linux CI without secrets: lint, build and tests for glow and wgpu, headless egui_kittest tests, Windows processes (Job Object) and Linux processes (pidfd, groups, cgroup where delegable), software rendering (WARP, llvmpipe with Wayland and X11), accelerator absent; explicit MSVC environment on the Windows runner; macOS build by F2 with a risk in `STATUS.md` if the runner is missing | sec. 10 F1; `F0-07` "Proposta: CI minima per F1"; `F0-01` §4.1 | CI logs; `docs/TESTING.md` | 0014, 0022 |
| F1-03 | Capability detection and diagnostics with usable errors: WHPX probe with `query-accelerators` (not with the CIM state of the `HypervisorPlatform` feature), clear error without an accelerator and without TCG fallback, verification of the hashes of the approved QEMU binaries | sec. 10 F1; D-6; ADR-0007, 0010 | F1 verification file; `missing-accelerator` job | 0007, 0010 |
| F1-04 | VM model, state machine, resource validation, approved media registry with a managed read-only copy, hashes and opening protected against reparse points (`CREATE_NEW`) | sec. 10 F1; sec. 6.3 | property tests; Host and File negative tests | 0005, 0008 |
| F1-05 | Windows process management in `qemu-backend`/`lightvm-service`: `CreateProcessW` with `JOB_LIST` and `HANDLE_LIST`, `DETACHED_PROCESS`, environment reduced to `SystemRoot`, identity by handle + creation time + `run_id`, classification of exit codes (including job limit notifications), escalation `system_powerdown` → `quit` → `TerminateJobObject` with a configurable timeout | `F0-06` Required actions and Proposed decisions | 20 real cycles (F1-12) | 0002, 0009 |
| F1-06 | **Single QMP owner**: asynchronous QMP client, one per execution, connected right after spawn, over a Unix socket in a private per-execution directory with a user-only ACL; multiplexing of commands, input, screenshots and events; timeouts; removal of the orphaned socket; state reconciliation from the process and QMP | sec. 10 F1; D-9; `F0-04` Actions 3 | tests with simulated QMP (declared) and with real QEMU | 0012 |
| F1-07 | Internal input and capture APIs: `screendump` in a private directory with the file decided by the backend, PPM validation and size limits; typed input with a scancode → qcode table for the declared layouts (US, IT) and key release | sec. 10 F1; `F0-04` §4 | unit tests; real test | 0003 |
| F1-08 | Offline profile and QEMU commands from controlled options: `-nodefaults -nic none` with post-start verification via `info network` and `query-pci` (not `query-rx-filter`), `-blockdev` with explicit format, protocols on an allowlist (excluding the build's remote drivers: `http`, `https`, `ftp`, `ftps`, `nfs`, `ssh`, `nbd`), rejection of paths with commas or non-absolute paths | sec. 10 F1; `F0-02` §3, §5; `F0-05` Procedure | "no unexpected NIC" criterion | 0004, 0010 |
| F1-09 | **Share mode on disks**: opening of the baseline and its vars with `FILE_SHARE_READ`, writer detector, overlay handle without `FILE_SHARE_DELETE`, verification of the baseline hash before every start; test of a per-execution directory with an explicit ACL and of a distinct process identity for QEMU for the overlay in use | `F0-05` §7, Actions 1; D-2, D-3 | F1 verification file | 0025 |
| F1-10 | Minimal audit of creation, start, shutdown and denials | sec. 10 F1 | audit log in tests | — |
| F1-11 | Supervisor skeleton and headless tests without UI; authenticated local IPC over a named pipe with ACL; UI that launches the supervisor as a separate process (prerequisite of P-2) | sec. 10 F1; ADR-0002 | headless integration tests | 0002 |
| F1-12 | F1 exit criterion: disk creation, at least 20 real start/stop cycles without orphaned processes, clear error with accelerator absent, no unexpected NIC, rejection of invalid paths and resources | sec. 10 F1 | F1 verification file on a physical runner | 0002, 0004, 0007 |
| F1-13 | **Blocking host clipboard reading**: egui-winit reads the clipboard on Ctrl+V even if the UI discards the text; configure or replace that path before F2 | D-13; `F0-04` Actions 5 | UI test and manual test | 0003 |
| F1-14 | **Linux packaging and X11 panic**: declare the dependencies loaded with `dlopen` (at least `libxkbcommon0`, `libxkbcommon-x11-0`, `libwayland-client0`, `libEGL`/`libGL` or `libvulkan1`); turn the `xkbcommon-dl` panic into a usable error | D-17; `F0-07` Actions 5 | CI job `smoke-render-software` | 0014 |
| F1-15 | **cgroup delegation**: creation of the delegated scope without external `systemd-run` (D-Bus `StartTransientUnit`) and behavior without a systemd user session, in a non-accelerated environment; preparation of the R2a tests | `F0-07` Actions 7; ADR-0015 part A | CI job `processes-linux` | 0015, 0017 |
| F1-16 | Automatic backend selection (glow → wgpu DX12 → WARP) with a visible indication; test in an RDP session | ADR-0014; limit L-1 | F1/F2 verification file | 0014 |
| F1-17 | CPU model with more extensions (`Skylake-Client-v4` or `Haswell-v4`) up to the desktop; installation of Ubuntu to disk and boot from disk | D-1; ADR-0011 criterion 2 | F1 verification file | 0011 |
| F1-18 | First measurements with product processes: UI + supervisor memory in private bytes (working set separately), manager CPU at idle over 5 minutes, 20 cold and 20 warm UI starts | sec. 13; limit L-3; `F0-09` §5 | row in `docs/TESTING.md` | 0014, 0018 |
| F1-19 | Closing the non-blocking F0 activities: MCP SDK and clients (ADR-0021), packaging and license inventory including the dependencies of `rfb-client` (ADR-0023), egui_kittest snapshots (ADR-0022), complete `COMPATIBILITY.md` and `TESTING.md` | sec. 10 F0 and F1 | dedicated verification files | 0021, 0022, 0023 |

### F1 progress

Updated on 2026-10-10 from reading the verification files and the code, then after the real tests of `F1-03` and `F1-09` on the same day. "Code present" does not count as verification (section 16, item 8): without a file in `docs/verification/` the item stays `not executed`. No item counts towards the F1 exit criterion until F0 is closed (section 10).

| ID | Status | Evidence or reason |
| --- | --- | --- |
| F1-01 | partial | types, schemas, errors and migrations verified with synthetic tests (`F1-01`, `F1-02`); build and tests in CI on GitHub not run |
| F1-02 | partial | full workflow validated locally (`F1-08`); never run on GitHub |
| F1-03 | verified on hardware (Windows) | `F1-03`: probe with approved hashes, unapproved hash rejected, accelerator absent → `unsupported` without TCG |
| F1-04 | partial | model and registry verified with synthetic tests (`F1-01`, `F1-02`); OS file identity and opening protected against reparse points not wired into the service (`TODO(F1)` in `lightvm-service/src/adapters/catalog.rs`) |
| F1-05 | verified on hardware (Windows) | `F1-03`: Job Object with one process, escalation `powerdown` → `quit` → kill, exit classification, PID + creation time identity, 20 cycles without orphans; parent loss with real QEMU in `F1-09` (no orphan, orderly shutdown not completed) |
| F1-06 | partial | QMP client verified with simulated QMP (`tests/qmp_mock.rs`) and real QMP (`F1-03`: orphaned socket removed, reconciliation, screendump, input); **the spike console in the UI still opens the QMP socket directly** (`apps/desktop/src/console.rs`, `qmp_worker`): it must be moved onto the supervisor IPC with the F2 display transport (ADR-0012, ADR-0013) |
| F1-07 | partial | `F1-03`: real screendump in a private directory, PNG, size limit, input with key release, geometry and `run_id` rejections; US/IT scancode table not tested with real QEMU |
| F1-08 | verified on hardware (Windows) | `F1-03`: 18 rejections without processes (paths, remote protocols, resources, CPU, machine), NIC detector with `info network` and `query-pci`, no NIC in 20 cycles |
| F1-09 | partial | storage part verified with synthetic tests (`F1-02`); `qemu-backend/src/storage_guard.rs` without evidence; overlay in use writable by another process of the same user (open decision) |
| F1-10 | verified with tests | audit of creation, start, shutdown, Emergency Stop and denials (`F1-01`; `F1-04`, `core_path`) |
| F1-11 | partial | headless supervisor and IPC verified (`F1-04`); the UI launches the supervisor as a child and uses IPC (`F1-09`); P-2 verified with the configurable policy (ADR-0015 B); Linux not run |
| F1-12 | run, not valid as exit | `F1-03`: disk creation, 20 real cycles without orphans, accelerator absent, no NIC, rejections verified on the backend APIs; the service now creates the primary disk (`vms/<vm_id>/primary.qcow2`). Also 20 cycles **through the service** (IPC → core → QEMU) without leftovers (`F1-03`, section of 2026-10-10). Does not count for the exit criterion while F0 is open |
| F1-13 | verified on hardware (Windows) | `F1-05` (section of 2026-10-10): egui-winit patched in `vendor/egui-winit`, no host clipboard reading (real Ctrl+V: 2 `Paste` events without the patch, 0 with the patch); app filter as a second defense; manual test with the console focused in P-4 |
| F1-14 | partial | Linux dependencies listed (`F1-07`); job `smoke-render-software` not run on GitHub; runtime verification not run |
| F1-15 | partial | D-Bus `StartTransientUnit` probe verified in WSL2 (`F1-08`); not implemented in `qemu-backend` (`TODO(F1/R2a)` in `process/mod.rs`) |
| F1-16 | partial | `F1-05`: auto → glow, fallback glow → wgpu DX12 and → WARP with indicator verified on hardware; RDP test not run |
| F1-17 | verified on hardware (Windows) | `F1-03` (sections of 2026-10-10): `Skylake-Client-v4` up to the live desktop; automatic installation to disk in 8 min; boot from disk to the login screen; ordinary ACPI shutdown in 3.6 s |
| F1-18 | partial | `F1-05`: UI + supervisor idle 5 min (glow: CPU ≤ 0.14 % of one core, private ≤ 106 MiB; wgpu: CPU ≤ 0.07 %, private ≈ 492 MiB, D-12, then **189 MiB with `MemoryHints::MemoryUsage`**), 20 warm starts per backend (first frame p95 139 ms glow, 248 ms wgpu); cold starts not run |
| F1-19 | partial | MCP SDK (`F1-06`, ADR-0021 accepted for the SDK), licenses (`F1-07`, ADR-0023 partially accepted); 10 egui_kittest snapshots verified on the same host (`F1-05`; ADR-0022 accepted) |

Workspace status (2026-10-10, Windows 11 host with Rust 1.99.0 MSVC): `cargo fmt --check`, `cargo clippy -D warnings` (workspace, `glow`/`wgpu` variants of the UI, service without `supervisor`) and `cargo test --workspace --locked` green; the latest count is in the most recent section of `F1-05` (the ignored ones are tests with real QEMU or manual tests, run separately in `F1-03`, `F1-09` and following).

### Open points from F0 for later phases

| Phase | Activity | Origin |
| --- | --- | --- |
| F2 | Display credential per execution and VM, distinct from QMP and IPC, delivered via IPC and revoked | ADR-0013 |
| F2 | VeNCrypt: test with `connections=` and supervisor timeout; shutdown of the TLS session | D-5 |
| F2 | DesktopSize, Hextile, CopyRect and Raw sent by a real QEMU; reduction of duplicate frames per update; `HighPerformance`/`LowPower` preference | `F0-04` Actions 6; ADR-0014 |
| F2 | AltGr with the IT layout and a real screen reader with AccessKit | `F0-04` Limits; ADR-0001 |
| F3 | Journal and reconciliation (`F0-05` §9), `.partial` files with atomic rename and comparison, 30 cycles with a real installation | `F0-05` Actions 2; ADR-0005, 0025 |
| F3 | Storage thresholds, reserve, screenshot and log limits, under-pressure tests; measurement of the effect of the I/O rate | ADR-0017; `F0-08` |
| F3 | Implementation and calibration of the fixture | ADR-0019 |
| F4 | Revocation and Emergency Stop with a concurrent input flood, 100 samples also with the guest at the desktop | `F0-06` Limits; sec. 13; D-18 |
| F6 | Additional provenance verification of the QEMU build (expired signature) | ADR-0010 |
| R2a | All Linux tests on a physical host listed in ADR-0016 | ADR-0016 |

## Open decisions

ADRs in Proposed status (details in `docs/adr/`):

| ADR | Decision | To be closed by | Why it remains open |
| --- | --- | --- | --- |
| 0015 | Termination of children on Linux (part A) | F0 → R2a | part B accepted on 2026-10-10; part A to be confirmed on a physical Linux host (ADR-0016) |
| 0017 | OS limits and storage budget | F3 | partially accepted on 2026-10-10: per-VM limits (`F1-10`); host thresholds, full accounting, under-pressure tests in F3; Linux in R2a |

Partially accepted: ADR-0021 (SDK `rmcp` 3.5.1 accepted; MCP clients open until F5) and ADR-0023 (policy and controls accepted; SBOM and egui fonts open until F6).

Accepted on 2026-10-10 by the project owner: ADR-0016 (verified on Windows only), ADR-0020 (signing and review not available: F6 release blocked), ADR-0022 (egui_kittest), ADR-0026 (authentication codes), ADR-0015 part B. ADR-0017: direction given by the project owner (limits in the VM settings, also configurable from the command line when opening a VM and from MCP), to be completed. Operational details of the approved data medium (section 17): recorded in ADR-0008 (P-7 closed on 2026-10-10).

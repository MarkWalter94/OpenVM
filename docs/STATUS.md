# Project status

Last updated: 2026-10-14

Operational document required by section 17 of `docs/ROADMAP.md`: current phase, impediments and progress. **This file does not change the Definition of Done (section 11), the scope or the exit criteria.** A status reported here does not count as verification: the evidence is in `docs/verification/`.

## Current phase

**F0 — Feasibility and decisions: under review** (roadmap, sections 10 and 15, step 6). The review `docs/verification/F0-09-evidence-review.md` concludes that the exit criterion is met with published deviations on the first Windows host, but F0 is **not yet closed**: the pending items listed below remain. The F0 tests concern feasibility spikes. The F1 work started in parallel is described in the section "F1 work started in parallel": some F1 product capabilities are `verified on hardware` on the first host, but none counts towards the F1 exit criterion until F0 is closed.

Classification used for each capability (section 16, item 6): `implemented`, `verified in simulation/test`, `verified on hardware`, `not verified`. The F1 product capabilities are `verified on hardware` or `verified in simulation/test` according to the F1 table below (F1 files); the feasibility tests on hardware are listed in the F0 files.

Section 10 rule: F1 starts after the F0 exit criterion. The F1 work started in parallel cannot be used to declare an F1 exit criterion before F0 is closed.

## F0 steps (section 15)

| Step | Work | Status | Outcome and evidence |
| --- | --- | --- | --- |
| 1 | Freeze scenario, threats, targets and dependencies; verify toolchain | completed with limits | `F0-01` (Windows build verified with an explicit MSVC environment; WARP verified; glow without acceleration failed; RDP not run); `F0-03` (Ubuntu 26.04.1 LTS, checksum and signature verified); initial ADRs; matrix in `COMPATIBILITY.md` |
| 2 | Real QEMU on Windows, WHPX probe, QMP and stop | completed with deviations | `F0-02`: QEMU 11.1.0 with verified provenance (expired Authenticode signature), WHPX active, QMP over a Unix socket, no NIC with `-nodefaults -nic none`, forced shutdown without orphans; boot to the live desktop only with `-cpu qemu64` (`-cpu host` failed); image locking failed |
| 3 | Native console with RFB client, input and screenshots on the protected channel | completed with a pending item | `F0-04`: internal RFB client over a private Unix socket verified, QMP input and screenshots verified (p95 19.2 ms); VeNCrypt verified with shutdown failed; physical keyboard through winit not run (P-4); second user not run (P-5) |
| 4 | Baseline/overlay/firmware and process supervision | completed with pending items | `F0-05`: 5 restore cycles with base and vars unchanged, exclusion of the base through share mode (ADR-0025), storage crash; `F0-06`: Job Object, supervisor death without orphans, Emergency Stop p95 640.2 ms (30 samples at the desktop) and 273.3 ms (100 samples during boot); UI crash not run (P-2) |
| 5 | Linux/KVM smoke test and resource controls | completed with outcome "lab unavailable" | `F0-07`: no physical Linux host; build and process mechanisms in WSL2 with limited validity; `F0-08`: Job Object CPU and memory limits verified, no hard storage quota without administrator rights |
| 6 | Evidence review | completed (proposal awaiting confirmation) | `F0-09`: continuation decision proposed; F1 backlog in `docs/RELEASE_PLAN.md` |

Section 15 constraint: no extensive dashboard construction before the outcome of QEMU, console and isolation on the first host. The outcome on the first host is now recorded in `F0-09`.

## F0 continuation decision (proposed)

**Windows 11 x86_64 remains the first target and the only MVP target, with the outcome "verified on Windows only"** (section 10 F0, paragraph "Linux lab unavailable"; ADR-0016). No check failed without an allowed alternative; no result uses TCG. Details of deviations (D-1…D-18) and pending items in `docs/verification/F0-09-evidence-review.md` §2–§3.

The decision becomes effective with the user's confirmation (U-5) and with the closure of pending items P-2 and P-4…P-7.

### Pending items for closing F0

| ID | Pending item | Who |
| --- | --- | --- |
| P-1 | Confirmation that no physical Linux host with KVM is available in F0 | **closed on 2026-10-10**: confirmed (U-5); physical Linux server obtained later, for R2a; ADR-0016 accepted |
| P-2 | UI crash on Windows (UI that launches the supervisor as a separate process); ADR-0015 part B decision | **closed on 2026-10-10**: ADR-0015 B accepted (setting, default terminate the VMs); test `F1-09` verified with both policies |
| P-4 | Physical keyboard → winit → egui → QMP, including AltGr, Ctrl+C/V, numeric keypad | user with development: the project owner runs it at the end of F1 development |
| P-5 | Test with a second local user on the private sockets, or decision to record it as a limit | **Linux part verified on 2026-10-10** in an Ubuntu VM (`F1-11`, validity [processes]): other user → `Permission denied`; with relaxed permissions → rejected by UID. **Windows part not run**: requires a second Windows account (created by the project owner) or recording it as a limit |
| P-6 | Availability of code signing and independent review (ADR-0020) | **closed on 2026-10-10**: not available (U-7); F6 release blocked while missing, ADR-0020 accepted |
| P-7 | Details of the approved data medium (ADR-0008) | **closed on 2026-10-10**: ISO 9660/UDF format verified, limits 16/8 GiB, read-only IDE CD-ROM, hash re-verified at every start (ADR-0008); writing from the guest verified impossible (`F1-11`) |

## F1 work started in parallel

Started before the closure of F0 and therefore **not usable for the F1 exit criterion** (section 10). Progress per backlog item in `docs/RELEASE_PLAN.md`, section "F1 progress".

| Stream | Crate | Evidence | Declared outcome |
| --- | --- | --- | --- |
| Shared types, VM model, states, resources, sessions, audit | `lightvm-proto`, `vm-core` | `F1-01` | verified in simulation/test |
| SQLite catalog, migrations, journal, media registry, disk guards | `vm-storage` | `F1-02` | verified in simulation/test (Windows) |
| Supervisor, authenticated IPC, composition root | `lightvm-service` | `F1-04` | verified in simulation/test (Windows, WSL2); end-to-end with real QEMU on WHPX, 2 runs, VM stopped in firmware |
| MCP SDK over stdio | `lightvm-mcp` | `F1-06` | verified in simulation/test; no real MCP client |
| Licenses and `cargo deny` | workspace | `F1-07` | verified |
| CI and cgroup delegation | `.github/workflows/ci.yml` | `F1-08` | validated locally; **never run on GitHub** |
| Probe, processes, QMP, input, screenshots, offline profile, 20 real cycles (also via the service), service disk, disk installation and boot from disk (F1-17) | `qemu-backend`, `lightvm-service` | `F1-03` | verified on hardware (Windows, WHPX) |
| UI that launches the supervisor and uses IPC; UI crash with a real VM | `apps/desktop`, `lightvm-service` | `F1-09` | verified on hardware; policy on UI loss as a setting (ADR-0015 B) |
| Per-VM limits (CPU, memory, storage budget), VM settings, `--open-vm` | `lightvm-proto`, `vm-core`, `qemu-backend`, `lightvm-service`, `apps/desktop` | `F1-10` | verified on hardware (Windows); full storage in F3 |
| UI: clipboard, rendering backend, snapshots, measurements | `apps/desktop` | `F1-05` | verified on hardware (rendering fallback, measurements, **host clipboard reading blocked** with patched egui-winit) and headless (snapshots); memory with wgpu within target after the D-12 fix (189 MiB) |

Situation at end of day (2026-10-10):

- **F1 exit criterion (F1-12)**: all items run with a positive outcome on the first host (`F1-03`: disk creation, 20 real cycles also through the service, accelerator absent, no NIC, rejections), but per section 10 **they do not count until F0 is closed**. To close F0, **P-4** (physical keyboard, script ready) and the **Windows part of P-5** (second Windows account or recording as a limit) remain.
- Resolved during the day: shutdown on UI loss (ADR-0015 B), host clipboard reading (F1-13, patched egui-winit), disk installation and boot from disk (F1-17), per-VM limits (ADR-0017 in part), details of the data medium (P-7), reconciliation on restart.
- Open in F1: cold start and RDP (F1-18, F1-16), CI never run on GitHub (F1-02), process containment on Linux (F1-15, R2a).
- The console no longer opens QMP directly: since F2 it goes through the supervisor with a per-run display ticket (`F2-01`, `F2-02`).

## F2 work started in parallel

Started on 2026-10-10 at the owner's request, before the closure of F0 and of the F1 exit criterion: like the F1 work, **not usable for the F2 exit criterion** until the previous phases are closed (section 10). Evidence in `docs/verification/F2-*`.

| Item (ROADMAP F2) | Evidence | Declared outcome |
| --- | --- | --- |
| Dashboard (OS, state, vCPU/RAM, virtual size, space used), wizard (name, OS, install media, data media, CPU/RAM/disk, network Offline only, summary), ISO import with the native picker | `F2-02`, `F2-01` | verified headless (kittest) and on hardware (wizard → import 6.4 GB ISO → create → start, `real_console`) |
| Console as a texture through the supervisor with a per-run display ticket (ADR-0013), input as IPC batches, full screen, screenshot, connection state | `F2-01`, `F2-02` | verified on hardware (framebuffer through the ticket, input p95 < 1 ms, ticket revoked on stop/Emergency Stop/connection loss); IT/US keyboard in a real guest and coordinates after resize by hand not executed (P-4) |
| Manual Ubuntu installation, boot from disk, persistence after reopening | `F2-01` | verified on hardware through the service (unattended install, media removed, boot from disk to the login screen, new service on the same catalog, boot again); **by a user following the guide: not executed** (exit criterion) |
| Graceful stop with timeout, distinct force stop, explicit reset | `F2-01`, `F2-02` | verified on hardware; graceful stop runs in the background, force stop wins |
| Delete dialog, close with running VMs | `F2-02`, `F2-01` | verified headless; deletion verified on hardware |
| Deletion of imported media (Settings → Installation media; IPC `delete_media`, UI role only, refused to agent sessions; refused while a VM configuration uses the medium; a managed copy through a journaled `delete_file` intent, refused while a run holds it, reconciliation after an interruption; a file used in place keeps existing, only its row goes, ADR-0029) | `vm-storage` `media_registry`, `vm-core` `f2_catalog`, `lightvm-service` `f2`/`f4`, `lightvm-desktop` `media_ui` | verified in simulation/test; not verified by hand in the app |
| Config changes only with the VM stopped; media detach | `F2-02`, `F1-10` | verified headless; baseline invalidation not applicable until F3 |
| Dark/light mode, resizable layout, keyboard operation, AccessKit, status not by colour only; Settings (QEMU paths, host capacity, security, network/MCP shown unavailable) | `F2-02` | verified headless; screen reader, DPI scaling and multi-monitor **not executed** (manual) |
| Experimental installer and first test on a clean machine | `F2-03` | per-user zip and Inno Setup installer verified on the development host; clean machine **not executed** (Windows Sandbox procedure ready) |
| macOS build on a runner (portability probe) | `F2-03`; `R2b-01` | GitHub job never run; on 2026-10-12 build, clippy and the whole test suite run on an Apple Silicon Mac (698 passed, 0 failed); portability defects fixed (IPC socket path fallback, media source opened with `O_NOFOLLOW | O_NONBLOCK` on Unix: links refused and FIFOs no longer block, Windows-only assumptions in tests) |

Decisions taken in F2: default CPU model `Skylake-Client-v4` (with `qemu64` the reference guest does not reach the desktop, deviation D-1); per-VM persistent UEFI variables; graceful stop asynchronous; user console input allowed while a VM is stopping; the wizard VM name is the only text field of the UI (it never reaches QEMU).

Open for the F2 exit criterion: a user follows `docs/USER_GUIDE.md`, installs Ubuntu, reopens LightVM and finds the VM; Italian/English keyboard and coordinates after resize checked by hand; screen reader, DPI and multi-monitor; clean-machine installer test.

## F3 and F4 work started in parallel

Started on 2026-10-11 at the owner's request; like F1 and F2, **not usable for their exit criteria** until the previous phases are closed (section 10).

| Item | Evidence | Declared outcome |
| --- | --- | --- |
| F3: baseline with the VM off (standalone image + UEFI variables + manifest), restore by overlay, deletion, crash reconciliation, quotas, audit; baseline screen | `F3-01`, `F3-02` | 30/30 modify-restore cycles on real QEMU (second run; first run 29/30 because of a guest kernel panic after the check); base hashes unchanged over 60 cycles; injected crashes at every critical point and a real service kill during `qemu-img convert` without loss; UI verified headless and with real QEMU |
| F3: deterministic fixture (ADR-0019) as a reproducible data ISO | `F3-01` | built by `crates/lightvm-fixture` (pure-Rust ISO 9660), verified in the 30 cycles |
| Start-time verification off the supervisor thread | `F2-01`, `F3-01` | verified on hardware (6.4 GB ISO, other requests p50 0.47 ms during verification) |
| F4: agent sessions (pairing, approval with code, one VM, capabilities, quotas, lease), input ownership, pause/revoke, Emergency Stop, local emergency endpoint, audit | `F4-01` | verified on real QEMU (agent-driven fixture through its own screenshots; revoke closes a flooding agent in < 1 ms, Emergency Stop ends QEMU p95 298 ms; stuck supervisor with full queue: 0 inputs forwarded) |

Deviations recorded in `F4-01` (D-1…D-6): service-side gate in addition to the core checks; **credential-less local emergency endpoint** (only verb Emergency Stop, same-user ACL; see ADR-0009); revocation keeps the pairing credential; lease expiry, client loss and UI loss stop the session's VM; screenshots in 32 KiB chunks; input ownership defaults to the user, one live session.

Open: the service does not report the last run id of a stopped VM (`VmSummary.run_id` is `None`), unlike its doc comment; the UI relies on disk usage for the install-media hint.

## F5 and F6 work started in parallel

Started on 2026-10-11 at the owner's request; not usable for the exit criteria until the previous phases are closed (section 10).

| Item | Evidence | Declared outcome |
| --- | --- | --- |
| F5: MCP stdio adapter `lightvm-mcp` (pairing file, verification code, tool catalogue v0.2 with `_` names, image content, schema-validated results) | `F5-01` | verified with a real service and a scripted client; **0 real LLM clients run** (Claude Code CLI logged out on this host; pi has no MCP support) |
| F5: session reports (JSON canonical, Markdown export by the user, outcome states with their source, typed text never stored) and AI Workspace / history / verdict / export UI | `F5-01`, `F5-02` | verified headless and with real QEMU |
| F5: deterministic fixture ten times with restore | `F5-03` | 10/10 runs completed and restored, 0 infrastructure errors (scripted client); failed, inconclusive, cancelled, timed_out and infrastructure_error produced on real QEMU |
| F6: migrations (backup, downgrade refusal, refusal with open runs), re-probe after QEMU/firmware change, disk-full handling, host resume, SBOM, licence check, release artefact scan, regression script | `F6-01` | verified in simulation/test (injected faults, injected clock jump, fake distribution); real full volume, real suspend and a second QEMU build not executed |

**Exit criteria still open (owner or external):**
- F0: P-4 (physical keyboard), Windows part of P-5 (second user).
- F2: a user installs Ubuntu following `docs/USER_GUIDE.md`; IT/US keyboard and coordinates after resize by hand; screen reader, DPI, multi-monitor; installer on a clean machine (Windows Sandbox procedure ready).
- F3: a human driving the baseline screen on an installed system.
- F4: a human driving the sessions UI; host sleep/resume for real.
- F5: two real LLM clients with a successful path each (Claude Code: `claude auth login`, then the F5 real-client test; a second client with native MCP stdio support to be chosen).
- F6: code signing and independent review (ADR-0020, not available), tests on at least two physical hosts (Intel/AMD if both declared), Linux (R2a).

## Catalog move (Settings)

**Settings → Supervisor → Move catalog…** moves the whole catalog (database, managed media copies, VM disks, baselines, overlays, firmware and TPM state, runs, session reports) to another folder; **Browse…** keeps only switching LightVM to another catalog and now says that existing VMs are not moved. The move runs in `lightvm-service --move-catalog` with the supervisor stopped (the UI does not open catalog files, ADR-0024) and is `vm_storage::move_catalog`: every VM stopped and no open journal intent, destination empty and outside the catalog, free space checked, rename on the same volume, otherwise copy with SHA-256 read-back of every file, then the destination opened and checked (`quick_check`, same VMs, media, baselines) before the source is removed; any failure or cancellation removes the partial destination and leaves the source unchanged.

| Item | Evidence | Declared outcome |
| --- | --- | --- |
| Move rules, failure in the middle, altered copy, cancel, lack of space, non-empty destination, destination inside the source, links, running VM | `crates/vm-storage/tests/catalog_move.rs` | verified in simulation/test |
| Child-process contract (JSON progress, exit codes) and real executable end to end | `crates/lightvm-service/src/catalog_move.rs` tests; `apps/desktop/tests/service_child.rs` | verified in simulation/test |
| Settings flow: availability, confirmation, progress with Cancel, settings updated only after success, supervisor restarted | `apps/desktop/tests/catalog_move_ui.rs` | verified in simulation/test |
| Move of a real catalog with installed VMs between two physical drives | — | not verified |

## ISOs used in place (ADR-0029)

Owner decision of 2026-10-14: a selected ISO is no longer copied into the catalog. New imports record the user's file in place (absolute path resolved by the system, size, SHA-256, guest detection through the same held handle; nothing copied, no temporary file, no journal intent); every start opens the file with `FILE_SHARE_READ` only, checks every path component against links, the volume and the final path, re-hashes it through that handle and gives QEMU that same path while the handle is held. Schema 8 (`0008_media_in_place.sql`) adds `storage` (`managed` | `referenced`) and `external_path`; media imported before keep their managed copy and keep working. Deleting a medium used in place removes only its registry row. ROADMAP 6.3 states this rule; it amends the copy rule of ADR-0008 (residual risks in ADR-0029 and `SECURITY.md`).

| Item | Evidence | Declared outcome |
| --- | --- | --- |
| Import in place (no file under `media/`, path recorded, same file and kind selected again updates its row), refusals (inside the catalog, path QEMU cannot take, a junction on the path at start), changed file refused at start naming the file, missing file `missing` and back, deletion keeps the file | `vm-storage` `media_registry`; `lightvm-service` `adapters::catalog` tests and `f2`; `vm-core` `scenarios`; `lightvm-desktop` `media_ui` | verified in simulation/test |
| Migration from schema 7 with a managed copy attached to a VM (backup, copy verified and deletable, constraints of the rebuilt table) | `vm-storage` `migrations` | verified in simulation/test |
| Catalog move with a medium used in place (not copied, reference kept) | `vm-storage` `catalog_move` | verified in simulation/test |
| Real Ubuntu/Windows ISO used in place through the app with real QEMU | — | not verified |

## R6 work: import of VMware VMs (ADR-0030)

Started on 2026-10-14 at the owner's request (ROADMAP 12, R6, in parallel; not a v1.0 requirement). **Dashboard → Import VMware VM** examines a powered-off VMware Workstation/Player VM (`.vmx` chosen with the picker, UI role only) and converts the current state of its main disk (snapshot chain flattened) into a new QCOW2 disk of the catalog; the VMware files are only read. The new crate `vmware-import` reads the `.vmx` and the VMDK descriptors as untrusted input; `qemu-backend` gained the `pvscsi` and `mptsas1068` controllers (`scsi-hd`), `vmware-svga` and an explicit `-smp` topology for imported VMs only (`DeviceProfile::for_imported`; the existing profiles' command lines are unchanged); `vm-storage` the journaled `import_vm` intent (schema 9, `0009_import_vm.sql`); `vm-core` the import checks (`vm_import` audit, name reserved during the import, imported hardware fixed afterwards); the service the preview/import/cancel requests with progress events. Evidence: `docs/verification/R6-01-vmware-import.md`.

| Item | Evidence | Declared outcome |
| --- | --- | --- |
| Parser: every VMDK create type of `qemu-img` (monolithicSparse, monolithicFlat, twoGbMaxExtentSparse, twoGbMaxExtentFlat, streamOptimized) and snapshot chains, cross-checked with `qemu-img info` | `vmware-import` `tests/fixtures.rs` (fixtures made at test time with the bundled `qemu-img`) | verified in simulation/test |
| Hostile `.vmx` and descriptors: traversal, absolute paths outside the folder, UNC and device paths, junctions and links, oversized files, encrypted, suspended, locked, unclean shutdown, unknown guest, looping/overlong/inconsistent chains, raw device mappings; parsers fuzzed | `vmware-import` `tests/hostile.rs`, `tests/fuzz.rs`, unit tests | verified in simulation/test |
| Mapping (guest, vCPU topology, RAM within the host limits, firmware, controllers, display, not imported, warnings) | `vmware-import` `plan` tests; `vm-core` `tests/vm_import.rs` | verified in simulation/test |
| Command lines of the imported controllers and display; existing profiles unchanged | `qemu-backend` `cmdline` tests (`imported_hardware_argv`, `profile_rows_exact_argv`) | verified in simulation/test |
| Conversion with the real `qemu-img` (chain with data in every level: same content as the source, source files unchanged), progress, cancellation | `qemu-backend` `tests/vmdk_convert.rs`; `lightvm-service` `tests/vmware_import.rs` | verified in simulation/test |
| Journal: interrupted conversion and crash after publishing rolled back at the next open, cancellation, name conflict at the commit; migration 8 → 9 | `vm-storage` `tests/vm_import.rs`, `tests/migrations.rs` | verified in simulation/test |
| Through the IPC: preview, import, progress events, refusals (locked VM, storage budget, path), cancel by the user, by the Emergency Stop and by the shutdown with nothing left; agent sessions refused | `lightvm-service` `tests/vmware_import.rs`, `tests/f4.rs` (role matrix) | verified in simulation/test |
| UI: entry, picker, preview with warnings, name, display choice, progress, cancel, errors | `lightvm-desktop` `tests/vmware_import_ui.rs`, snapshots | verified in simulation/test |
| LSI Logic Windows guests: automatic offline enablement of the storage driver | ADR-0030; `docs/verification/R6-02-lsi-driver-spike.md` | implemented and verified on the real Windows Vista guest: the storage helper (an approved, separate GPL image, SHA-256) enables the guest's own driver in the converted copy only — stop 0x7B unprepared, desktop prepared on the product AHCI controller; without the helper or outside the covered combinations, warning and manual preparation (`USER_GUIDE.md` §6.3) |
| Real VMware VMs (Windows with PVSCSI, LSI Logic and SATA; Linux) imported and started to the desktop | `docs/verification/R6-01b-vmware-import-real.md` | partial: one Linux VM to the session entry (source unchanged, content identical, no orphans, audit), one suspended VM correctly refused; Windows Vista on LSI Logic running (activation dialog) after the automatic storage driver preparation; Linux Mint with a 14-level chain to the desktop; the legacy-BIOS NVMe Windows 10 VM now gets a controller SeaBIOS boots, but the guest resets itself in early boot under WHPX (open) |

## R2c work: Windows guests

Started on 2026-10-12 at the owner's request (ADR-0027); it does not change the MVP scope or any exit criterion, and R2c is not closed by it.

| Item | Evidence | Declared outcome |
| --- | --- | --- |
| Scope: Windows XP, Vista, 7, 8.1, 10 guests through the normal wizard, ISO detection confirmed by the user, offline only; Windows 8 and 11 not supported | ADR-0027 | decided; implementation in progress |
| SeaBIOS (`bios-256k.bin`, pinned hash) under WHPX with `Skylake-Client-v4` on `pc-q35-11.1` and `pc-i440fx-11.1` | `R2c-01` | verified with the Ubuntu ISO only (boot loader and kernel reached, no `Unexpected VP exit`) |
| Detection and Windows Setup from real media: Vista x86 (BIOS), Windows 10 x64 (UEFI, CD prompt answered by the backend) and x86 (BIOS) | `R2c-01` | verified up to the first Setup page |
| Windows Vista x64 on Windows hosts | ADR-0027, `R2c-01` | not supported: `STOP 0x5C` under WHPX; refused by `vm-core` and the wizard |
| Guest stopped on a virtual CPU error (`WHPX: Unexpected VP exit code 4`, a triple fault; QEMU leaves it in `internal-error`), seen at Vista x86's first Setup reboot | `R2c-01` (2026-10-14) | handled: automatic `system_reset` + `cont` as on hardware, at most 3 in 120 s, audited (`vm_auto_reset`); past the limit the Dashboard and console say so and offer reset/force stop. Verified with synthetic QMP and UI tests; the Vista installation with the new build is not yet verified |
| Install, reboot, pointer/keyboard, shutdown, baseline/restore for each Windows system | `R2c-01` (pending) | not verified: no Windows installation completed; XP, 7 and 8.1 media missing |
| Ubuntu/KVM host for Windows guests (R2c) | — | not verified (no physical Linux host, ADR-0016) |

The Windows cells of `docs/COMPATIBILITY.md` stay "not verified". Windows installation media and licences are provided by the user.

## R2b work: macOS Apple Silicon and Windows 11 ARM64

Started on 2026-10-12 at the owner's request (ADR-0028, Accepted on 2026-10-12 with evidence partial). It does not change the MVP scope or any exit criterion.

| Item | Evidence | Declared outcome |
| --- | --- | --- |
| Windows 11 ARM64 setup under HVF passes the requirements check (TPM 2.0 via swtpm, Secure Boot) without bypass | `R2b-01` | verified up to the licence terms page; installation, OOBE offline, reboot, baseline not executed |
| Project-built AArch64 Secure Boot firmware (edk2-stable202408 + one patch) | `R2b-01`; `packaging/firmware/edk2-aarch64/` | build script verified from an empty directory; release hashes not yet recorded |
| QEMU containment without cgroups (watchdog, kqueue, process group) | `R2b-01` | verified with a test harness: 0 orphans in 60 cycles, p95 9.89 ms; not in the product |
| macOS host layer of the product (private directories, host resources, handle path, process group + lifeline watchdog) and the Windows 11 ARM64 profile through the service | `R2b-01` §5 | verified on hardware up to Windows Setup (ignored test `macos_real`); installation, OOBE, baselines with TPM state not done; development hash approvals only |

## Linux lab outcome

**"Verified on Windows only"**, confirmed by the project owner on 2026-10-10 (ADR-0016); the physical Linux server for R2a will be obtained later. With this outcome Windows is the only MVP target, the Ubuntu cell of `docs/COMPATIBILITY.md` stays "not verified" and Linux qualification is deferred to R2a, which does not close without the test on a physical host. WSL2 was used only for builds and accelerator-independent tests (`F0-07`).

## F0 operational documents

| Document | Status |
| --- | --- |
| `docs/ARCHITECTURE.md` | updated with the F0 outcomes (console, channels, process model) |
| `docs/SECURITY.md` | controls "planned", with feasibility proven where indicated; F0 residual risks added |
| `docs/COMPATIBILITY.md` | Windows 11 row → Linux "experimental" with the F0 data; Ubuntu row "not verified"; completed in commit `aa04ccd` |
| `docs/TESTING.md` | F0 measurements recorded; egui_kittest tested; completed in commit `aa04ccd`; F1-18 measurements missing |
| `docs/RELEASE_PLAN.md` | F0 backlog with outcomes; F1 backlog with per-item progress |
| `docs/adr/` | 21 ADRs Accepted, of which 0016 conditional on user confirmation and 0021, 0023 partially accepted; ADRs Proposed or partially open on 2026-10-10: 0015 part A (Linux, R2a), 0017 (per-VM limits accepted; host thresholds and full storage in F3); accepted on 2026-10-10: 0015 part B, 0016, 0020, 0022, 0026; ADR-0027 (Windows guest hardware profiles) Accepted on 2026-10-12 with evidence pending |

## Impediments and open points

| # | Impediment | Status | Effect | Reference |
| --- | --- | --- | --- | --- |
| I-1 | QEMU missing at start | resolved | QEMU 11.1.0 installed portably with verified provenance | `F0-02` §2; ADR-0010 |
| I-2 | WHPX status not verified | resolved with deviation | WHPX working; `HypervisorPlatform` feature reported *Disabled* via CIM (D-6); `-cpu host` does not boot OVMF (D-1) | `F0-02` §1, §9 |
| I-3 | Physical Linux host with KVM not available | closed for F0 | outcome "verified on Windows only" (ADR-0016); physical server for R2a obtained later | `F0-07`; ADR-0016 |
| I-4 | Path canonicalization warnings | not reproduced | no action unless they reappear | `F0-01` §8 |
| I-5 | Code signing and independent review not available | declared (2026-10-10) | does not block F1–F5; **blocks release in F6** | ADR-0020 |
| I-6 | eframe behavior without graphics acceleration (RDP) | partially open | WARP verified as degraded mode; RDP not run, limit declared | `F0-01` §6; ADR-0014 |
| I-7 | MSVC build requires the Build Tools 2022 environment to be loaded explicitly | open | development and CI must set up the environment; local fix at the user's choice | `F0-01` §4.1 |
| I-8 | No second local account for multi-user negative tests | open | P-5 | `F0-04` §6 |
| I-9 | Host shared with other agents during measurements | declared | F0 latencies are not release measurements | `F0-06`, `F0-08` |
| I-10 | Current development host without the Rust 1.99.0 toolchain and without QEMU 11.1.0; Visual Studio 2022 Community without the MSVC x64 desktop libraries | resolved | Rust 1.99.0 installed (rustup, without modifying PATH), MSVC x64 component added (UAC), QEMU 11.1.0 and ISO downloaded with hashes identical to F0 | `F1-03` §Ambiente |

Silence about an impediment does not equal its resolution (section 14, last line).

## Actions required from the user

Full list with origin in `docs/verification/F0-09-evidence-review.md` §8.

| ID | Action | Needed for |
| --- | --- | --- |
| U-1 | As administrator: check and if needed enable the `HypervisorPlatform` feature, then repeat the probe and `-cpu host` | optional (D-1, D-6) |
| U-2 | Provide a physical Ubuntu LTS Linux host with KVM, if available | alternative to U-5 |
| U-3 | Create a second non-administrator local account for the tests on the private sockets, or decide to record the limit | P-5 (answer: "in a VM", see P-5) |
| U-4 | Test with a physical keyboard in an unlocked session | P-4 — ready: `powershell -ExecutionPolicy Bypass -File apps\desktop\scripts\keyboard-test-u4.ps1` (instructions in the script; dry run on 2026-10-10: console with the Ubuntu desktop after about 100 s) |
| U-5 | Confirm the outcome "verified on Windows only" | P-1 — **done on 2026-10-10** |
| U-6 | In WSL: `sudo apt install build-essential pkg-config` (and possibly `libxkbcommon-x11-0`) | standard Linux build (non-blocking) |
| U-7 | Name those responsible for code signing and independent review, or declare their unavailability | P-6 — **done on 2026-10-10**: nobody |
| U-8 | Optional: smoke test of `lightvm-desktop` in an RDP session to the host | limit L-1 |
| U-9 | Decide on the fix for the local MSVC environment | I-7 — on the current host the MSVC x64 component was added to VS 2022 (2026-10-10) |
| U-10 | Optional, as administrator: query the NTFS quotas | ADR-0017 |
| U-11 | Optional and not valid for the matrix: nested KVM probe in WSL | integration only |

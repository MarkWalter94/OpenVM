# Recorded verifications

This directory contains the tests run on LightVM: procedures, results, benchmarks and artifact manifests. It is the only place where a "verified" state can be demonstrated. References: `docs/ROADMAP.md`, sections 13 and 17.

## Rules

1. **Every verification records** the command or procedure, environment, result and limits (section 13). A test that cannot be reproduced from the text of the file does not count as a verification.
2. **Three distinct outcomes, never merged** (section 17): `verified`, `failed`, `not executed`. An item not executed does not become verified by analogy, by deduction or because "it compiles".
3. **Explicit scope.** The file declares the host, build, versions and configuration for which the outcome holds. A result on one host does not extend to other hosts, accelerators, machines or guests (section 5).
4. **Mock and real boot kept separate.** A test with a simulated supervisor or QEMU is recorded as such and does not replace the test on hardware (sections 10 F1, 13, 16 point 5).
5. **Failures included.** Measurement samples, attempts with LLM clients and repeated tests also report errors, timeouts and failed attempts (sections 10 F5 and 13).
6. **No secrets.** No credentials, tokens, private keys or sensitive personal paths in the reported logs; raw logs stay in `lab/`, which is outside the repository.
7. **Identified artifacts.** ISOs, QEMU binaries, firmware and builds cited are identified by name, version, origin and hash (SHA-256).
8. **A file is not rewritten to change its outcome.** A new test produces a new file or a dated section appended at the end; the previous outcome remains readable.
9. **Decisions stay in the ADRs.** The verification file may propose a decision; the decision is recorded in `docs/adr/`, the status in `docs/STATUS.md`, the matrix in `docs/COMPATIBILITY.md`.

## Naming convention

```text
F<phase>-<nn>-<topic>.md
```

- `<phase>`: number of the phase in section 10 (`0`–`6`); for later milestones `R1`, `R2a`, etc.
- `<nn>`: two-digit sequence number within the phase.
- `<topic>`: lowercase words separated by hyphens, without spaces or accents.

Examples: `F0-01-toolchain-eframe.md`, `F0-02-qemu-whpx-qmp.md`, `F1-04-start-stop-cycles.md`.

## Verification index

| File | Step / phase | Declared outcome | File status |
| --- | --- | --- | --- |
| `F0-01-toolchain-eframe.md` | F0, step 1 (section 15) | per item: Windows build verified with explicit MSVC environment; glow without acceleration failed; RDP and Linux not executed in this file | complete |
| `F0-02-qemu-whpx-qmp.md` | F0, step 2 (and base/overlay of step 4) | per item: WHPX, QMP over Unix socket, NIC, forced stop verified; image locking and `-cpu host` failed | complete |
| `F0-03-guest-media.md` | F0, steps 1–2 (guest release, checksum) | verified | complete |
| `F0-04-console-rfb-input.md` | F0, step 3 | partial | complete |
| `F0-05-baseline-overlay-firmware.md` | F0, step 4 (baseline, overlay, firmware, disk exclusion) | partial | complete |
| `F0-06-process-supervisor.md` | F0, step 4 (process supervision) | partial (UI crash not executed) | complete |
| `F0-07-linux-kvm.md` | F0, step 5 (Linux/KVM) | partial; smoke test on physical host not executed | complete |
| `F0-08-resource-limits-measurements.md` | F0, step 5 (resource limits and consumption) | partial | complete |
| `F0-09-evidence-review.md` | F0, step 6 (review, go/no-go decision) | partial; F0 not yet closed | complete |
| `F1-01-proto-core.md` | F1 (backlog F1-01, F1-04 as model, F1-10) | partial: all items verified only with synthetic tests | complete |
| `F1-02-storage.md` | F1 (backlog F1-01 migrations, F1-04 media registry, F1-09 storage part) | partial: verified with synthetic tests on Windows; real QEMU and Linux not executed | complete |
| `F1-04-service-ipc-ci.md` | F1 (backlog F1-10, F1-11; composition root integration) | partial: IPC, single instance, revocation, parent death verified (Windows, WSL2); end-to-end with real QEMU, 2 runs; second user, parent loss with real QEMU and CI on GitHub not executed | complete (dated section at the end) |
| `F1-06-mcp-sdk.md` | F1 (backlog F1-19, MCP SDK) | partial: `rmcp` SDK tested over stdio; no real MCP client | complete |
| `F1-07-licences-packaging.md` | F1 (backlog F1-19 licences, F1-14 packaging) | verified for inventory and `cargo deny`; SBOM and Linux runtime libraries not executed | complete |
| `F1-08-ci.md` | F1 (backlog F1-02, F1-15) | partial: workflow validated locally, not run on GitHub; cgroup delegation tested in WSL2 | complete |
| `F1-03-real-qemu-backend.md` | F1 (backlog F1-03, F1-05…F1-08, F1-12) | partial: probe, rejections, NIC, 20 start/stop cycles without orphans and service disk verified on hardware (Windows, WHPX); VM in the firmware; does not count as exit criterion while F0 is open | complete |
| `F1-09-ui-supervisor-ui-crash.md` | F1 (backlog F1-11); F0 P-2 pending | partial: UI → child supervisor → IPC verified; UI crash without orphans verified, orderly VM shutdown failed (supervisor timeout); Linux not executed | complete |
| `F1-10-vm-limits-settings.md` | F1 (ADR-0017; backlog F1-04, F1-05) | partial: CPU and memory limits applied to QEMU and verified; VM editing via IPC, UI and command line; storage budget only at startup; fixes to normal shutdown and to reconciliation on restart | complete |
| `F1-11-second-user-and-data-disc.md` | F0 P-5 (Linux part), P-7 | partial: second user rejected (permissions and UID) in Ubuntu VM [processes]; writing the data CD from the guest impossible; Windows part not executed | complete |
| `F2-01-service-console-media.md` | F2 (supervisor: media import, deletion, console ticket, input, persistent UEFI variables, asynchronous stop; exit path through the service) | partial: verified on hardware through the service; by a user via the UI not executed | complete (dated sections at the end) |
| `F2-02-desktop-ui.md` | F2 (dashboard, wizard, console, dialogs, settings, accessibility) | partial: verified headless and with real QEMU (wizard → console); screen reader, DPI, multi-monitor, real keyboard not executed | complete |
| `F2-03-packaging-guide-ci.md` | F2 (experimental installer, user guide, macOS portability probe) | partial: package verified on the development host; clean machine and macOS job not executed | complete |
| `F3-01-baseline-restore.md` | F3 (baseline with the VM off, restore by overlay, crash recovery, fixture) | partial: 30/30 cycles on real QEMU (second run), base hashes unchanged, crash tests at every critical point including a real service kill; Linux, installed-Ubuntu path and a human user not executed | complete |
| `F3-02-baseline-ui.md` | F3 (baseline screen) | partial: verified headless and through the UI with real QEMU and qemu-img (empty disk); installed system, screen reader not executed | complete |
| `F4-01-sessions-bridge.md` | F4 (sessions, capabilities, quotas, input ownership, pause/revoke, Emergency Stop, emergency endpoint) | partial: verified on real QEMU (agent-driven fixture, stop latencies with a flooding client, stuck supervisor with full queue); Linux, real MCP client, host sleep/resume not executed | complete |
| `F4-02-sessions-ui.md` | F4 (Agents page, approval with code, session bar, input ownership in the console) | partial: verified headless and with real QEMU and a scripted agent; human user not executed | complete |
| `F5-01-mcp-reports.md` | F5 (MCP stdio adapter, agent calls, session reports) | partial: real adapter and real QEMU with a scripted client; real LLM clients not executed | complete |
| `F5-02-reports-ui.md` | F5 (AI Workspace, history, report page, verdict, export) | partial: verified headless; real agent through the UI not executed | complete |
| `F5-03-fixture-runs.md` | F5 (fixture ten times with restore; every outcome state) | verified with a scripted client on real QEMU (10/10, 0 infrastructure errors); LLM clients not executed | complete |
| `F6-01-stabilization.md` | F6 (migrations, re-probe, disk full, host resume, SBOM, licences, artefact scan, regression) | partial: verified in simulation/test; signing, independent review, second host, real suspend/full volume not executed | complete |
| `F1-05-desktop-egui.md` | F1 (backlog F1-13, F1-16, F1-18, F1-19) | partial: clipboard without text in the UI (headless) but read in egui-winit not blocking; rendering fallback verified; UI + supervisor measurements (wgpu above the memory target); RDP, cold start and manual test not executed | complete |
| `R2c-01-windows-guest-profiles.md` | R2c (Windows guest hardware profiles, ADR-0027) | partial: SeaBIOS + WHPX with `Skylake-Client-v4` on `pc-q35-11.1` and `pc-i440fx-11.1` verified with the Ubuntu ISO; every Windows guest test not executed (pending list) | complete |
| `R2b-01-macos-hvf-windows11-arm64.md` | R2b (macOS Apple Silicon, ADR-0028) | partial: Windows 11 ARM64 setup under HVF passes the requirements check with swtpm and the project-built Secure Boot firmware; watchdog containment 0 orphans in 60 cycles; installation, OOBE and baseline not executed | complete |
| `R6-01-vmware-import.md` | R6 (import of VMware VMs, ADR-0030) | partial: parser, hostile inputs, mapping, journal, conversion with the real `qemu-img`, IPC and UI verified in test; real VMware VMs partially executed (see `R6-01b`) | complete |
| `R6-01b-vmware-import-real.md` | R6 (real VMware VMs, continuation of `R6-01`) | partial: one real Linux VM to the session entry (source unchanged, content identical, no orphans), one suspended VM refused; NVMe + legacy-BIOS boot-failure finding; Vista and Mint not executed | in progress |
| `R6-02-lsi-driver-spike.md` | R6 (offline storage-driver preparation for LSI Logic Windows guests) | in progress: technique and driver source identified; execution pending | in progress |

## Template

Copy the following block into a new file and fill it in. The sections are mandatory; if a section does not apply, state so explicitly.

```markdown
# F<phase>-<nn> — <title>

- Execution date (UTC): YYYY-MM-DD
- Executed by: <name or role>
- Roadmap step: section <n>, <phase/step>
- Related ADRs: ADR-NNNN, ...
- Overall outcome: verified | failed | partial | not executed

## Purpose

What the test intends to demonstrate and which roadmap criterion (section and item) it feeds.
What this test does NOT demonstrate.

## Environment

| Item | Value |
| --- | --- |
| Host (model, CPU, RAM, storage) | |
| OS and build | |
| Accelerator and state | |
| Toolchain / relevant versions | |
| QEMU binary (version, origin, SHA-256) | |
| Firmware (file, version, SHA-256) | |
| Guest / media (name, version, SHA-256) | |
| Graphics session (local, RDP, headless) | |
| Other conditions (antivirus, group policies, etc.) | |

## Procedure

Numbered steps, with the exact commands (code blocks) and the configuration files used.
No secrets in the reported commands.

## Results

| Item | Outcome | Evidence |
| --- | --- | --- |
| <check 1> | verified / failed / not executed | log excerpt, hash, screenshot, measurement |

For measurements: number of samples, p50, p95, maximum, timeouts and errors (section 13).

## Limits

Conditions not covered, differences from the reference environment, observed anomalies not explained.

## Required actions

Activities the test makes necessary, with a reference to the backlog in `docs/RELEASE_PLAN.md`.

## Proposed decisions

Proposals for the related ADRs (they are not decisions until the ADR is updated).
```

# Testing strategy

Last updated: 2026-10-09 (F0 review; F1 work in progress: CI, MCP SDK, licences)

Operational translation of section 13 of `docs/ROADMAP.md`. The F0 feasibility tests (spikes) are recorded in `docs/verification/F0-*` and summarised in `F0-09-evidence-review.md`; the first tests of the F1 crates (with simulated backend, catalog or QMP, declared as such) are in `docs/verification/F1-*`. Some F1 product capabilities are verified on hardware on the first host (`F1-03`, `F1-05`, `F1-09`, `F1-10`), without counting towards the F1 exit criterion while F0 is open (`docs/STATUS.md`). Procedures and outcomes must be recorded in `docs/verification/` following the template in `docs/verification/README.md`. This document does not report test counts: the numbers live in the verification files that measured them.

## 0. Test levels

| Level | What it contains | Where it lives | What it does not prove | Current examples |
| --- | --- | --- | --- | --- |
| Unit | pure logic, states, policies, validation, parsers with fixtures | `#[cfg(test)]` modules of the crates | integration between processes, OS, QEMU | `vm-core`, `vm-storage`, `lightvm-proto`, `qemu-backend` (`probe`, `cmdline`, `validate` parsers) |
| Headless integration | several real components or processes without UI or accelerator: service IPC, MCP server on stdio as a child process, SQLite on temporary files | `crates/*/tests/` | real QEMU, rendering, acceleration | `lightvm-service/tests/ipc.rs` (`F1-04`), `lightvm-mcp/tests/stdio.rs` (`F1-06`) |
| Instrumented UI tests | egui interface driven via AccessKit with egui_kittest, simulated supervisor | `apps/desktop/tests/` | real IPC, native rendering, acceleration | `kittest_ui.rs`, `service_ui.rs`, `vm_settings_ui.rs`, snapshots in `visual_snapshots.rs` (§4) |
| Declared mocks | simulated QMP, QEMU, backend or catalog, stderr/help fixtures recorded from real QEMU | `crates/qemu-backend/tests/qmp_mock.rs`, `tests/fixtures/`; fake backend and catalog of `lightvm-service` | real boot, devices, latencies (§2) | classification of the no-accelerator error from fixtures (job `missing-accelerator`) |
| Processes without accelerator | Job Object on Windows; pidfd, groups, delegated cgroup on Linux, with test children | crate tests and spikes; jobs `processes-windows`, `processes-linux` | QEMU with KVM/WHPX, limits on the guest | `F0-06`, `F0-07` §4, `F1-08` §3 (**[processi]** in WSL2) |
| Real tests | QEMU with verified provenance and accelerator on a physical host; `#[ignore]` tests launched by hand or by physical runners | `apps/desktop/tests/real_console.rs`, `crates/qemu-backend/tests/real_qemu.rs` (ignored by default), scripts in `lab/` | combinations other than the one tested | F0 tests on Windows (`F0-02`…`F0-06`) |

The verification files classify each outcome as mock/simulation or hardware (section 16, point 6).

## 1. Verification pipeline

| Level | When | Content | Where it runs | Evidence |
| --- | --- | --- | --- | --- |
| Per change | On every change | Meaningful unit tests, validation of the generated JSON schemas, lint, build of the configured targets, licences and advisories | CI without secrets, `.github/workflows/ci.yml`: `lint`, `build-test-windows`, `build-test-linux`, `processes-windows`, `processes-linux`, `smoke-render-software`, `missing-accelerator`, `schema`, `cargo-deny`, `macos-check` (`F1-08`; **not run yet**: repository not published) | CI logs |
| Properties and adversarial input | On every change to the affected areas | Property tests on VM transitions and on the storage registry; adversarial input and targeted fuzzing of the exposed parsers (IPC, MCP, RFB, manifest) | CI | CI logs, fuzzing corpus |
| Controlled physical runners | On changes to backend, console, storage, supervisor; before every milestone | Probe, accelerated boot, QMP, console, restore, negative tests that require a real VM | Physical lab hosts with accelerator | Files in `docs/verification/` |
| Pre-release | Before every release | Clean install, upgrade, E2E regression, negative tests on the final artefact | Clean hosts of the target | Files in `docs/verification/` |
| Per host | Before enabling a host | Full real suite on that host, not just cross-compilation | Physical host of the target | Updated row in `docs/COMPATIBILITY.md` |

Avoid tests that merely repeat the implementation (section 13).

## 2. Tests with mocks and tests with real boot

| Type | Allowed for | Does not prove |
| --- | --- | --- |
| QEMU/QMP mock | Errors, timeouts, protocol, reconciliation, state transitions | Real boot, accelerator, devices, latencies |
| Simulated supervisor in UI tests | Layout, flows, accessibility via AccessKit | Real IPC, native rendering, acceleration |
| Real boot on a physical runner | Phase exit criteria, negative tests on VMs, measurements | Combinations other than the one tested |

Rules:

- Mocks and fixtures only in tests; real boot remains an acceptance requirement (section 16, point 5; F1 exit criterion).
- Every verification file declares whether the outcome comes from a mock, a simulation or hardware (classification of section 16, point 6).
- No screen with simulated data in the product path (F2 exit criterion).

## 3. Test organisation

| Directory | Content |
| --- | --- |
| `crates/*/src` (`#[cfg(test)]` modules) | Pure logic, states, policies, validation |
| `crates/*/tests/`, `apps/desktop/tests/` | Per-crate integration: service IPC, MCP server on stdio, storage, simulated QMP, UI with egui_kittest; tests with real QEMU marked `#[ignore]` |
| `tests/integration/` | Integration across several crates (headless supervisor with real or simulated backend, declared); today only `README.md` |
| `tests/security/` | Negative tests of section 7.4 (checklist in `docs/SECURITY.md`); today only `README.md` |
| `tests/e2e/` | Complete scenarios with a real VM and, from F5, MCP clients; today only `README.md` |

Naming conventions used by the CI: `missing_accelerator` for the capability detection tests without accelerator (F1-03, job `missing-accelerator`; already present in `qemu-backend`) and `delegated_cgroup` for the `#[ignore]` tests of cgroup delegation that the supervisor will add (F1-15, job `processes-linux`). The job emits a warning when the filter finds no tests.

The deterministic graphical fixture is defined in ADR-0019 and implemented by F3.

## 4. UI test framework

**egui_kittest 0.36.2** is the chosen framework (ADR-0022; version aligned with egui/eframe 0.36.2 of ADR-0014): it drives the interface through the AccessKit tree and, with the `snapshot` feature, compares graphical snapshots. The ADR is accepted (2026-10-10); the snapshots for criterion 4 are in `F1-05`. Tested in F0, without GPU and without a native window:

| Test | Host | Outcome | Evidence |
| --- | --- | --- | --- |
| Navigation and theme switch via AccessKit role and label (3 tests) | Windows 11 | verified | `F0-01` §7 |
| No text fields in the sections; real console configured; UI → key table → QMP → guest path with real QEMU (test `real_console`, ignored by default) | Windows 11 | verified | `F0-04` Results §4, §6 |
| `cargo test --workspace` including 4 `kittest_ui` tests | Linux in WSL2 (**[build]**) | verified | `F0-07` §2 |
| Graphical snapshots (`snapshot` feature) | — | 10 snapshots in `apps/desktop/tests/visual_snapshots.rs` (panels, Dashboard, VM settings, Settings dark and light, console), tolerance in `kittest.toml` | `F1-05` (sections F1-19 and new visual system) |
| Execution in CI (Windows and Ubuntu, runners without GPU) | GitHub runner | not run | `F1-08` |

- Use role plus label to locate nodes (`get_by_role_and_label`): the label alone can match several nodes (`F0-01` §7).
- UI tests are accompanied by headless core tests and, when they use a simulated supervisor, are labelled as such: they do not verify IPC, native rendering or acceleration.
- Automation hooks only in test artefacts; the public artefact does not include automation drivers, test endpoints or debug settings (section 10 F6).
- Instrumented UI tests and tests on the real installer have separate evidence.

## 5. Performance measurements

Preliminary targets of section 13: to be measured, not results obtained. A deviation is published with its cause and planned remedy, without changing the targets. Blocking: missing measurements, orphan processes, unaccounted growth, violation of functional or authorization limits.

Reference conditions: physical host ≥ 4 cores/8 threads, 16 GiB RAM, SSD; Linux guest 2 vCPU/4 GiB; display 1280×720; ≥ 100 GiB free (section 13; measurement host proposed in ADR-0018).

First row of each measurement: F0 values on spikes (not release measurements), ADR-0018 host shared with other agents. Proposed memory metric for the target: private bytes on Windows, `RssAnon + VmSwap` on Linux; working set and `VmRSS` published separately (`F0-09` §5).

Note (2026-10-11): `--smoke`, `--static`, `--simulated-console` and `--no-supervisor` are measurement switches available only in builds with the `dev-tools` feature (`cargo build --release -p lightvm-desktop --features dev-tools`); the distributed package does not accept them (`F6-01`). The measurements below were taken before the gating, with the same code paths.

| Measurement | Target (sec. 13) | Method | Samples | Host / backend | p50 | p95 | Maximum | Timeouts / errors | Outcome | Evidence |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| Manager CPU at idle | mean < 1% of one logical core | F0: 10 s windows (`--static`) and 60 s (console with guest stopped), UI only | 1 per configuration | Windows 11; wgpu / glow | — | — | — | — | not run in the required form (5 minutes, UI + supervisor); indications: wgpu 0.05–0.16%, glow 0.18–0.47% | `F0-01` §5; `F0-04` §7 |
| Manager CPU at idle | mean < 1% of one logical core | F1: UI + supervisor (no VM), `--static`, 311 s window (second series; first series 271 s) | 2 series per backend | Windows 11 (i9-14900K); glow / wgpu DX12 | — | — | glow 0.141 % (series 1: 0.006 %); wgpu 0.025 % (series 1: 0.069 %); supervisor 0 % | 0 | within target | `F1-05` |
| Manager memory — glow | ≤ 250 MiB | F0: UI only with active console, 60 s; private bytes (working set separately) | 1 series per condition | Windows 11; glow | — | — | private 97.6 MiB; working set 94.5 MiB | — | UI alone within target; supervisor not measured | `F0-04` §7 |
| Manager memory — wgpu DX12 | ≤ 250 MiB | as above | 1 series per condition | Windows 11; wgpu DX12 | — | — | private 372.0 MiB; working set 198.3 MiB | — | deviation D-12 (private bytes above target with the UI alone) | `F0-04` §7 |
| Manager memory — glow (F1) | ≤ 250 MiB | F1: UI + supervisor without VM, idle 5 min, samples every 5 s; private bytes (working set separately) | 62 samples | Windows 11; glow | — | — | private UI 103.0 + supervisor 2.4 MiB (total max 105.5); working set 73.3 + 11.0 MiB | 0 | within target | `F1-05` |
| Manager memory — wgpu DX12 (F1) | ≤ 250 MiB | as above | 62 samples | Windows 11; wgpu DX12 | — | — | private UI 488.0 + supervisor 2.4 MiB (total max 491.8); working set 185.3 + 11.0 MiB | 0 | deviation D-12 confirmed with the supervisor | `F1-05` |
| Manager memory — wgpu DX12 with `MemoryHints::MemoryUsage` | ≤ 250 MiB | UI alone, idle 30 s, release | 6 samples × 2 runs | Windows 11; wgpu DX12 / WARP | — | — | DX12 189.4 MiB; WARP 188.2 MiB (before: 488.6 MiB) | 0 | **within target**: D-12 resolved | `F1-05` (section D-12) |
| Manager memory — wgpu WARP | ≤ 250 MiB | spike without console, reading at 3 s | 1 | Windows 11; WARP | — | — | private 376.3 MiB; working set 124.8 MiB | — | deviation, degraded mode | `F0-01` §5 |
| UI start (cold) | p95 ≤ 3 s | On SSD | 20 | — | — | — | — | — | not run (requires a reboot or a privileged cache flush) | `F1-05` |
| UI start (warm) | p95 ≤ 3 s | F0: first frame from entry into `main`; full process with `--smoke 0.5` reported separately | 20 (glow, wgpu Vulkan); 10 (wgpu DX12) | Windows 11 | first frame: glow 294, Vulkan 705, DX12 501 ms | glow 326, Vulkan 919, DX12 538 ms (full process: glow 653, Vulkan 1333 ms) | glow 347, Vulkan 1005, DX12 538 ms | 0 | within target on the warm sample | `F0-01` §5 |
| UI start (warm) | p95 ≤ 3 s | F1: release binary, first frame from entry into `main`; full process with `--smoke 0.5` | 20 per backend | Windows 11 (i9-14900K) | first frame: glow 117, wgpu DX12 239 ms (process: 633, 700 ms) | glow 139, wgpu DX12 248 ms (process: 642, 732 ms) | glow 140, wgpu DX12 414 ms (process: 645, 739 ms) | 0 | within target | `F1-05` |
| Guest screenshot | p95 ≤ 500 ms at 1280×720 | F0: `screendump` → PPM → PNG in memory, local guest, 1280×800 | 120 (still); 100 (animation) | Windows 11, QEMU 11.1.0/WHPX | 17.6 / 16.4 ms | 19.2 / 19.3 ms | 20.5 / 24.3 ms | 0 / 0 | within target | `F0-04` §7 |
| Revocation | p95 ≤ 250 ms | From the local action to blocking new forwards in the supervisor | ≥ 100 | — | — | — | — | — | not run (F4) | — |
| Emergency Stop — guest at desktop | p95 ≤ 2 s to QEMU process end | F0: `TerminateJobObject` until observed exit and empty job, wait of 125–140 s after start | 30 | Windows 11, QEMU 11.1.0/WHPX | 394.3 ms | 640.2 ms | 652.0 ms | 0 / 0 | within target; fewer than 100 samples (D-18) | `F0-06` §2c |
| Emergency Stop — guest booting | p95 ≤ 2 s to QEMU process end | as above, wait of 5–60 s after start | 100 | Windows 11, QEMU 11.1.0/WHPX | 142.7 ms | 273.3 ms | 354.5 ms | 0 / 0 | within target | `F0-06` §2c |
| Orphan processes | zero | F0: forced kills, supervisor death, external kills of QEMU, Emergency Stop, escalating shutdowns, baseline cycles | 5 + 10 + 6 + 130 + 10 + 5 cycles | Windows 11 | — | — | — | — | zero on the cycles run; UI crash not run | `F0-02` §7; `F0-05`; `F0-06` |
| Temporary space | no unaccounted growth | F0: per-session accounting (overlay, vars, logs, screendump, sockets) | — | Windows 11 | — | — | — | — | first accounting; session series and cleanup after reboot not run | `F0-08`; `F0-05` §9 |
| Console latency in the fallback (if used) | no numeric target; measured and declared (sec. 6.4) | F0: `screendump` capture only (delivery via IPC does not exist) | 120 | Windows 11 | 17.6 ms | 19.2 ms | 20.5 ms | 0 / 0 | fallback not needed on the first host | `F0-04` §7 |

Measurement rules (section 13):

- For latencies report p50, p95, maximum, timeouts and errors; failures are not excluded.
- Memory measurements distinguish working set, private bytes or equivalent metrics per OS; do not compare different metrics under the same label.
- Different rendering backends per OS require per-OS comparisons, without hiding child processes.
- Installer size reported separating included components, required downloads and VM space.
- Record the host conditions (power, power profile, background processes); with hybrid CPUs state the normalisation used (ADR-0018).

## 6. Rules for runners with untrusted guests

- No production credentials, signing keys or publishing tokens on runners that execute untrusted guests and images (section 13).
- Untrusted PRs do not automatically access the lab with secrets.
- Media and images used on runners come from the registry with a verified hash; no unverified download during the test.
- Offline profile on runners too: the test guest has no NIC.
- Runner logs and artefacts are cleaned of secrets before archiving in `docs/verification/`; raw logs stay out of the repository (`lab/`).
- WSL2, nested VMs or runners without verifiable acceleration serve only for builds and accelerator-independent tests (section 10 F0); tests of this kind are labelled **[build]** or **[processi]** as in `F0-07`.
- F1 CI (F1-02): workflow in `.github/workflows/ci.yml`, described and validated locally in `F1-08` (starting proposal in `F0-07`, "Proposta: CI minima per F1"). No CI runner has a verifiable accelerator or the approved QEMU: the physical lab jobs (`lab-fisico-windows`, `lab-fisico-linux`) stay out of the public CI and are not triggered by untrusted PRs.
- Licences and advisories on every change with `cargo-deny` (`deny.toml`, ADR-0023, `F1-07`).
- MCP SDK tested with a test client on stdio (`F1-06`); tests with real MCP clients belong to F5 and to `tests/e2e/`.
- macOS build on a suitable runner by F2, without claiming operational support: non-blocking `macos-check` job; if the runner is missing, the risk is recorded in `docs/STATUS.md` (section 10 F1).

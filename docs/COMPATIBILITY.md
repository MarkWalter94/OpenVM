# Compatibility

Last updated: 2026-10-09

Operational document required by section 17 of `docs/ROADMAP.md`: versions and combinations **actually tested**. **It does not claim support without evidence.** The combinations and release commitments are those of section 5; this document only records the outcome of the tests.

Current status: F0 under review (`docs/verification/F0-09-evidence-review.md`); F1 work started in parallel, which does not claim exit criteria before F0 is closed. Only one combination has recorded tests: Windows 11 x86_64 → Linux x86_64 with WHPX, status **experimental** (F0 feasibility tests passed on spikes, with deviations; no product capability implemented). All other cells are "not verified". The outcome "verified on Windows only" is **proposed and awaiting the user's confirmation** (section "F0 outcome on the targets").

Two distinct vocabularies: the **matrix cells** use the statuses of section 5 (legend below); the tables of **components and test environments** use the outcomes of the verification files (verified, failed, not run) plus "not verified" for anything without any test and "experimental" for partial positive tests with known limits. A "verified" outcome on a component does not promote any matrix cell.

## Status legend

Allowed statuses (section 5):

| Status | Meaning |
| --- | --- |
| not verified | No recorded test, or test not run. Initial status of every cell. |
| experimental | Recorded test with a partial positive outcome; known limits; cannot be advertised as supported. |
| supported | All tests required for that combination and mode passed and archived in `docs/verification/`. |
| blocked | Test run and failed on a mandatory requirement, with no allowed alternative; reason recorded. |

"Certification" means an internal qualification based on the published tests, not an external certification (section 5). A cell is updated only with a reference to a verification file. No cell is considered supported by analogy with another (section 12, R2d).

## Combination matrix

Columns required by section 5. The machine type must be recorded in versioned form (for example `pc-q35-<version>`).

| Combination (sec. 5) | Status | Host OS / build | CPU | QEMU binary and version | Accelerator | Machine type | Firmware | Guest | Console | Snapshot | Security controls | Limits | Last test passed |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| Windows 11 x86_64 → Linux x86_64 (WHPX) | experimental | Windows 11 Pro 10.0.26200 (build 26200); Hyper-V and VBS/HVCI active | Intel Core i7-12800HX, 16 cores / 24 logical processors (F0); Intel Core i9-14900K, 32 logical processors, same Windows build (F1-03, F1-09) | QEMU 11.1.0 (qemu.weilnetz.de, portable extraction), `qemu-system-x86_64.exe` SHA-256 `47d57a60…d0b3cd` (ADR-0010) | WHPX (`query-accelerators` → `enabled: whpx`); no TCG | `pc-q35-11.1`; CPU `qemu64`, 2 vCPU, 4 GiB | OVMF `edk2-x86_64-code.fd` (SHA-256 `33090cc0…6dbb2a`) in read-only pflash + per-run copy of `edk2-i386-vars.fd` (SHA-256 `5d2ac383…be5d1e`); Secure Boot off | Ubuntu 26.04.1 LTS desktop amd64, SHA-256 `601e30fb…1bda1f`, live session | RFB over private Unix socket (internal client, read-only); QMP screenshot p95 19.2 ms at 1280×800 | baseline with VM off with per-run overlay and vars: 5 cycles with base unchanged (feasibility) | QMP and VNC over Unix sockets with user-only ACL; refusal at low integrity; no NIC; Job Object; see `SECURITY.md` | `-cpu host` does not start OVMF; boot only from live ISO (installation to disk not tested); ACPI shutdown not completed by the live session; QEMU image locking ineffective (exclusion in the supervisor); QEMU Authenticode signature expired; VeNCrypt shutdown failed; second user and physical keyboard not tested; when the UI is lost the VM is not shut down in an orderly way within the supervisor timeout (`F1-09`) | 2026-10-10: `F1-03` (20 real start/stop cycles, VM in firmware), `F1-09` (UI → supervisor, UI crash); 2026-10-09: `F0-02`, `F0-04`, `F0-05`, `F0-06` (feasibility tests, not product tests) |
| Ubuntu LTS x86_64 → Linux x86_64 (KVM) | not verified | not verified | not verified | not verified | not verified | not verified | not verified | not verified | not verified | not verified | not verified | no physical Linux host in F0; WSL2 tests not valid for this cell (`F0-07`) | none |
| Debian/Fedora x86_64 → Linux x86_64 (KVM) | not verified | not verified | not verified | not verified | not verified | not verified | not verified | not verified | not verified | not verified | not verified | not verified | none |
| macOS Apple Silicon → Linux ARM64 (HVF) | not verified | not verified | not verified | not verified | not verified | not verified | not verified | not verified | not verified | not verified | not verified | not verified | none |
| macOS Intel → Linux x86_64 (HVF) | not verified | not verified | not verified | not verified | not verified | not verified | not verified | not verified | not verified | not verified | not verified | not verified | none |
| Windows/Linux x86_64 → Windows x86_64 (WHPX/KVM) | not verified | not verified | not verified | not verified | not verified | not verified | not verified | not verified | not verified | not verified | not verified | not verified | none |
| ARM64 host → x86_64 guest (TCG) | not verified | not verified | not verified | not verified | not verified | not verified | not verified | not verified | not verified | not verified | not verified | not verified | none |
| Compatible Apple hardware → macOS | not verified | not verified | not verified | not verified | not verified | not verified | not verified | not verified | not verified | not verified | not verified | not verified | none |
| Windows ARM64 → ARM64 guest | not verified | not verified | not verified | not verified | not verified | not verified | not verified | not verified | not verified | not verified | not verified | not verified | none |

Notes:

- The "Console" column states the actual mode: RFB over a private channel, RFB with VeNCrypt X.509, or screendump fallback (ADR-0003, ADR-0013).
- The "Snapshot" column states the actual model: in the MVP only baseline with VM off with overlay (ADR-0005).
- The "Security controls" column refers to the control table in `docs/SECURITY.md` with the status for that host.
- The TCG row stays outside the AI profile in any case (ADR-0007).
- The Ubuntu row stays "not verified" if F0 closes with the outcome "verified on Windows only" (section 10 F0; ADR-0016). WSL2, nested VMs and runners without verifiable acceleration do not update it.
- The "experimental" status of the Windows row reflects F0 feasibility tests passed with known limits; it cannot be advertised as supported and does not imply any mode of the following matrix.

## Mode matrix

Modes are tracked separately: manual support of a host does not imply the AI modes (section 5).

| Combination | Manual | Baseline | AI offline | Filtered Internet | Guest agent |
| --- | --- | --- | --- | --- | --- |
| Windows 11 x86_64 → Linux x86_64 (WHPX) | not verified | not verified | not verified | not verified | not verified |
| Ubuntu LTS x86_64 → Linux x86_64 (KVM) | not verified | not verified | not verified | not verified | not verified |
| Debian/Fedora x86_64 → Linux x86_64 (KVM) | not verified | not verified | not verified | not verified | not verified |
| macOS Apple Silicon → Linux ARM64 (HVF) | not verified | not verified | not verified | not verified | not verified |
| macOS Intel → Linux x86_64 (HVF) | not verified | not verified | not verified | not verified | not verified |
| Windows/Linux x86_64 → Windows x86_64 (WHPX/KVM) | not verified | not verified | not verified | not verified | not verified |

Filtered Internet belongs to R1 and guest agent to R4: their cells stay "not verified" for the whole MVP.

For Windows 11 → Linux the modes stay "not verified" because none is implemented in the product. The F0 feasibility tests cover only the technical prerequisites: manual management (boot, console, input: `F0-02`, `F0-04`) and baseline (`F0-05`); for AI offline, sessions, bridge and negative tests are missing (F4–F5).

## UI rendering per host

Required by the risk "eframe rendering missing on hosts without graphics acceleration" (section 14; ADR-0014).

| Host | Backend (wgpu/glow) | egui/eframe version | Local session | Remote session (RDP or equivalent) | Software fallback | Status | Evidence |
| --- | --- | --- | --- | --- | --- | --- | --- |
| Windows 11 x86_64 | glow default (OpenGL 3.3); wgpu DX12 selectable; WARP via wgpu as degraded mode (ADR-0014) | egui/eframe 0.36.2 | verified with glow, wgpu Vulkan/DX12/GL (NVIDIA GPU) | not run (declared limit) | wgpu on WARP verified: CPU about 7.9 logical processors with animation at 10 Hz, about 5.5 updates/s; glow without acceleration fails (`egui_glow requires opengl 2.0+`) | experimental | `F0-01` §5–§6; `F0-04` Results §3, §7 |
| Ubuntu LTS x86_64 | not verified | not verified | not verified | not verified | not verified | not verified | only WSL2/WSLg with llvmpipe (`F0-07` §3), not valid for this row; X11 path panics because `libxkbcommon-x11.so` is missing; libraries to be declared in the package in `F1-07` §6; software smoke in CI defined but not run (`F1-08`) |

## Console: mode and latency

If the screendump fallback is used, the update latency must be measured and declared here (section 6.4), with the method of section 13.

| Host | Console mode | Samples | p50 | p95 | Maximum | Timeouts / errors | Status | Evidence |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| Windows 11 x86_64 | RFB over private Unix socket (primary); screendump fallback measured but not needed | end-to-end screendump capture: 120 (guest still) / 100 (animation) | 17.6 / 16.4 ms | 19.2 / 19.3 ms | 20.5 / 24.3 ms | 0 / 0 | experimental | `F0-04` Results §7 (measured at 1280×800; IPC delivery of the fallback does not exist yet) |
| Ubuntu LTS x86_64 | not verified | — | — | — | — | — | not verified | — |

## External components tested

| Component | Version | Provenance | SHA-256 | Host | Status | Evidence |
| --- | --- | --- | --- | --- | --- | --- |
| QEMU (`qemu-system-x86_64`, `qemu-img`, `qemu-io`) | 11.1.0 (`v11.1.0-12130-ge470268ff4`) | `https://qemu.weilnetz.de/w64/qemu-w64-setup-20260811.exe` (published SHA-512 verified; SHA-256 matching the winget manifest); Authenticode signature expired | `qemu-system-x86_64.exe` `47d57a6072e0bb3bd98f87926eb129eb1736dfe818c67b3b81ef7ce4edd0b3cd`; `qemu-img.exe` `9d930582d0d574bb106d89982c547688e10b8f356192d302cc3d0ae4e8674534`; `qemu-io.exe` `2b4bce81239a3982d3e274a1409251a8c67b26876ff07e5ce921017e2d42da9a` | Windows 11 | experimental | `F0-02` §2–§3; ADR-0010 |
| UEFI firmware (OVMF/edk2, from the QEMU build) | `edk2-x86_64-code.fd`; template `edk2-i386-vars.fd` | `share/` of the QEMU 11.1.0 build | code `33090cc07675baa5190d9f1e84bf5176b33bcbfa9bacac522961150cdb6dbb2a`; vars `5d2ac383371b408398accee7ec27c8c09ea5b74a0de0ceea6513388b15be5d1e` | Windows 11 | experimental | `F0-02` §2.3, §9; `F0-05` §6; ADR-0011 |
| Ubuntu LTS guest (ISO) | 26.04.1 LTS desktop amd64 | `releases.ubuntu.com/26.04.1/`, `SHA256SUMS` signed by key `8439 38DF 228D 22F7 B374 2BC0 D94A A3F0 EFE2 1092` | `601e30fbf5d97759367c632e2c33630665039b7e2158fd068403da3ccf1bda1f` | Windows 11 (live boot under WHPX) | experimental | `F0-03`; `F0-02` §9; ADR-0011 |

## MCP SDK and MCP clients tested

Example configurations only for clients actually tested, with version and date (section 9; ADR-0021). **No real MCP client tested.**

Chosen SDK (ADR-0021): `rmcp` 3.5.1, stdio transport, negotiation limited to revision 2025-11-25. The test uses a JSON-RPC client written for the test and fake tools: it demonstrates the server's behaviour, not interoperability with clients.

| Test | MCP revision | Host OS | Outcome | Evidence |
| --- | --- | --- | --- | --- |
| `rmcp` 3.5.1 stdio server: `initialize` → `tools/list` → `tools/call`, structured results, operational and protocol errors kept distinct, versions other than the baseline not accepted | 2025-11-25 | Windows 11 Pro 10.0.26200 | verified (test client, fake tools) | `F1-06` |
| Same test | 2025-11-25 | Linux in WSL2 (**[build]**) | verified (test client, fake tools) | `F1-06` |
| Image content in results | 2025-11-25 | — | not run | — |

| Client | Version | Host OS | LightVM SDK version | Verification date | Outcome | Evidence |
| --- | --- | --- | --- | --- | --- | --- |
| — | — | — | `rmcp` 3.5.1 | — | not verified | — (F5) |

## Build and test environments (outside the matrix)

Environments without a verifiable accelerator: they serve accelerator-independent builds and tests and **do not update any matrix cell** (section 10 F0; `docs/TESTING.md` §6).

| Environment | Use | Outcome | Evidence |
| --- | --- | --- | --- |
| Windows 11 x86_64, Build Tools 2022 with explicit MSVC environment, Rust 1.99.0 | workspace build and tests | verified (with the MSVC environment loaded; without it, `LNK1104`) | `F0-01` §4.1; `F1-04` |
| Windows 11 x86_64, Visual Studio Community 2022 (MSVC 14.44, x64 component added), Rust 1.99.0 | workspace build, clippy and tests; real tests with WHPX | verified (before the component was added: `LNK1104 msvcrt.lib`) | `F1-03` |
| Linux in WSL2 (Ubuntu, kernel 6.18, zig linker) | build and tests; processes (pidfd, groups, delegated cgroup) | verified **[build]**/**[processi]** | `F0-07`; `F1-04`; `F1-06`; `F1-08` §3 |
| GitHub runners `windows-latest`, `ubuntu-latest` | F1 CI (`.github/workflows/ci.yml`) | not run (repository not published) | `F1-08` |
| GitHub runner `macos-latest` | only `cargo check`, no support claimed | not run | `F1-08` |
| Physical Ubuntu LTS | rendering smoke and `dlopen` libraries of the Linux package | not verified | `F1-07` §6 (list derived from the features, not tested at runtime) |

## F0 outcome on the targets

**Proposed: "verified on Windows only"**, awaiting the user's confirmation (ADR-0016; `docs/verification/F0-09-evidence-review.md` §6). With this outcome Windows is the only MVP target, the Ubuntu row stays "not verified" and Linux qualification is deferred to R2a. Allowed values (section 10 F0): Windows and Linux verified; verified on Windows only; Linux first target; blocked.

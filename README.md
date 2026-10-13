# LightVM

LightVM is a local virtual machine manager for repeatable tests, built on QEMU.
It ships a native Rust desktop UI (egui/eframe) with a built-in console and baselines that can be restored while the VM is off.
Control can be delegated to external agents over MCP (stdio), with authorization enforced by the backend, never by the UI.
Every agent session is confined to the VMs, tools, duration and quotas the user approved, and can be revoked at any time.
First supported host: Windows 11 x86_64 with Ubuntu LTS guests. No cloud service, telemetry or account is required.

## Status

**All MVP features are implemented (phases F1–F6 as far as they can be done on one host); no phase exit criterion is formally closed yet.** The roadmap phases are sequential and F0 (feasibility) still has two owner items open (P-4 physical keyboard, Windows part of P-5); each later phase also has manual or external items listed in `docs/STATUS.md`.

What works today on the first host (Windows 11 + WHPX, Ubuntu 26.04.1 LTS guest), each with evidence under `docs/verification/`:

- **Manual alpha (F2):** wizard with ISO import, dashboard, console in the app through the supervisor with a per-run display ticket, IT/US keyboard, full screen, graceful/forced stop, reset, deletion, persistent UEFI state; Ubuntu installed, booted from disk and found again after reopening (through the service).
- **Baselines (F3):** baseline with the VM off, restore by overlay, crash recovery; 30/30 modify-restore cycles with unchanged base hashes.
- **Agent sessions (F4):** pairing, approval with a verification code, one VM, capabilities, quotas, lease, input ownership, pause/revoke, Emergency Stop and a local emergency command; revoke closes a flooding agent in under 1 ms.
- **MCP and reports (F5):** `lightvm-mcp` stdio adapter (MCP 2025-11-25), session reports in JSON/Markdown with evidence-backed outcomes; 10/10 deterministic fixture runs on real QEMU with a scripted client. **Real LLM clients not run yet.**
- **Stabilization (F6, partial):** experimental per-user installer, SBOM, licence check, migration safety, disk-full handling, host-resume handling, regression script. Code signing and independent review are not available (ADR-0020): public distribution is blocked.
- **Windows guests (R2c, experimental):** hardware profiles for Windows XP, Vista, 7, 8.1 and 10 with ISO detection, offline only (ADR-0027); Windows Setup reached from real Vista 32-bit and Windows 10 media, no installation completed yet (`docs/verification/R2c-01-windows-guest-profiles.md`). Windows 8 and 11 are not supported; Windows Vista 64-bit is not supported on Windows hosts.

## Repository layout

| Path | Content |
| --- | --- |
| `apps/desktop` | Native desktop UI (`lightvm-desktop`), console texture fed by the RFB client |
| `crates/lightvm-proto` | Identifiers, states, public errors, IPC envelope, generated JSON Schemas |
| `crates/guest-detect` | Guest system detection from installation ISOs (ISO 9660, UDF, WIM metadata), bounded parser (ADR-0027) |
| `crates/vm-core` | VM model, state machine, resource limits, agent sessions, audit |
| `crates/vm-storage` | SQLite catalog, migrations, journal, media registry, disk guards |
| `crates/qemu-backend` | QEMU process control, QMP client, Job Object, screenshots, input |
| `crates/lightvm-service` | Supervisor process: IPC server, composition root, audit sink |
| `crates/lightvm-mcp` | MCP stdio server spike |
| `crates/rfb-client` | Read-only RFB 3.8 client used by the UI |
| `spikes/` | F0 feasibility probes, kept for reference |
| `docs/` | Roadmap, status, architecture, security, ADRs, verification evidence |

## Install (alpha)

An experimental per-user package for Windows 11 x64 is built with `packaging/windows/build-package.ps1` (output in `dist/`, not in the repository): `LightVM-<version>-windows-x64.zip` with its SHA-256 file, and a per-user Inno Setup `setup.exe` when Inno Setup 6 is installed on the build machine. It installs into `%LOCALAPPDATA%\Programs\LightVM` without administrator rights and never touches your VMs on uninstall. **The binaries are not code-signed** (ADR-0020) and **QEMU is not included**: QEMU 11.1.0 is installed separately and verified against the approved hashes (ADR-0010). Step-by-step instructions, requirements (virtualization and Windows Hypervisor Platform) and known limitations: [`docs/USER_GUIDE.md`](docs/USER_GUIDE.md). Packaging evidence: [`docs/verification/F2-03-packaging-guide-ci.md`](docs/verification/F2-03-packaging-guide-ci.md).

## Usage

Start **LightVM** from the Start Menu, point **Settings → QEMU distribution** to the QEMU folder, then create a VM with the wizard on the Dashboard, import the Ubuntu ISO, start it and install Ubuntu in the built-in console. The VMs are stored in `%LOCALAPPDATA%\LightVM\catalog` and are found again when LightVM is reopened. The full walkthrough, from requirements to Emergency Stop and deleting a VM, is in [`docs/USER_GUIDE.md`](docs/USER_GUIDE.md).

## Building

Requirements: Rust 1.99.0 (pinned in `rust-toolchain.toml`). On Windows the MSVC linker needs the Visual Studio 2022 Build Tools environment loaded (`vcvars64.bat`) before running cargo.

```
cargo build --workspace --locked
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Scripts that do the same with the right environment, and the full regression with a summary:

| Host | Build (and run) | Regression |
| --- | --- | --- |
| Windows (PowerShell) | `.\scripts\build.ps1 [-Release] [-DevTools] [-Run]` | `.\scripts\regression.ps1 [-Release] [-NoDebugInfo]` |
| macOS, Linux | `scripts/build.sh [--release] [--dev-tools] [--run [-- args]]` | `scripts/regression.sh [--release] [--no-debuginfo]` |

On macOS (Homebrew): `brew install rustup`, then `rustup toolchain install 1.99.0`; the scripts find the keg-only rustup by themselves. The app, the catalog and the media registry work on macOS; starting VMs does not yet (ADR-0028).

Tests that need a real QEMU are marked `#[ignore]` and enabled through environment variables documented in the corresponding verification file. QEMU, firmware and guest images are not part of the repository.

## Documents

| Document | Content |
| --- | --- |
| [`docs/ROADMAP.md`](docs/ROADMAP.md) | Frozen reference: scope, mandatory controls, phases and exit criteria. Not edited during execution; integrity in `docs/ROADMAP.sha256`. |
| [`docs/STATUS.md`](docs/STATUS.md) | Current phase, progress, impediments |
| [`docs/RELEASE_PLAN.md`](docs/RELEASE_PLAN.md) | Backlog, execution order, open decisions |
| [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) | Components, boundaries, process model, console |
| [`docs/SECURITY.md`](docs/SECURITY.md) | Threat model, controls, negative tests, residual risks |
| [`docs/COMPATIBILITY.md`](docs/COMPATIBILITY.md) | Matrix of the combinations actually tested |
| [`docs/TESTING.md`](docs/TESTING.md) | Verification pipeline and performance measurements |
| [`docs/adr/`](docs/adr/README.md) | Architecture decision records (accepted and proposed) |
| [`docs/verification/`](docs/verification/README.md) | Evidence of the tests that were run |

Project documentation is written in English, except the frozen `docs/ROADMAP.md`, which stays in Italian.

## How to read `docs/verification/`

Each file `F<phase>-<nn>-<topic>.md` documents one test: purpose, environment, procedure with exact commands, results and limits. Every item is marked `verified`, `failed` or `not executed`. Only a verification file can show that something works: a claim elsewhere without a reference to one does not count as evidence.

## License

Original code is released under the MIT License, see [`LICENSE`](LICENSE). QEMU, firmware, guest images and third-party dependencies carry their own licences, which the LightVM licence does not replace.

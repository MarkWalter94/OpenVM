# LightVM

LightVM is a local virtual machine manager for repeatable tests, built on QEMU.
It ships a native Rust desktop UI (egui/eframe) with a built-in console and baselines that can be restored while the VM is off.
Control can be delegated to external agents over MCP (stdio), with authorization enforced by the backend, never by the UI.
Every agent session is confined to the VMs, tools, duration and quotas the user approved, and can be revoked at any time.
First supported host: Windows 11 x86_64 with Ubuntu LTS guests. No cloud service, telemetry or account is required.

## Status

**Phase F1 (foundations and QEMU control) is in progress; F0 (feasibility) is under review.**

Feasibility work on the first host (Windows 11 with WHPX, Ubuntu 26.04.1 LTS guest) demonstrated accelerated boot, a native RFB console, input, control channels on private sockets, baseline restore and process supervision, with published deviations (for example CPU model `qemu64` instead of `host`). The proposed F0 outcome is "verified on Windows only" and still awaits user confirmation.

F1 streams delivered so far, each with its evidence file under `docs/verification/`:

- Shared types and JSON Schemas (`lightvm-proto`), VM state machine, resource validation, sessions and audit (`vm-core`).
- SQLite catalog with versioned migrations, journal of intents, approved-media registry and disk guards (`vm-storage`).
- Supervisor service with authenticated local IPC, real composition root and a headless end-to-end run against real QEMU (`lightvm-service`).
- MCP server spike on the official Rust SDK pinned to protocol revision 2025-11-25 (`lightvm-mcp`), licence inventory, CI workflow validated locally.

Still open in F1: the QEMU backend evidence (20 real start/stop cycles), the desktop UI evidence (clipboard isolation, renderer selection, first measurements), and the consolidation of `docs/STATUS.md`. CI has not run on GitHub yet. No product feature is considered verified until its evidence file says so.

## Repository layout

| Path | Content |
| --- | --- |
| `apps/desktop` | Native desktop UI (`lightvm-desktop`), console texture fed by the RFB client |
| `crates/lightvm-proto` | Identifiers, states, public errors, IPC envelope, generated JSON Schemas |
| `crates/vm-core` | VM model, state machine, resource limits, agent sessions, audit |
| `crates/vm-storage` | SQLite catalog, migrations, journal, media registry, disk guards |
| `crates/qemu-backend` | QEMU process control, QMP client, Job Object, screenshots, input |
| `crates/lightvm-service` | Supervisor process: IPC server, composition root, audit sink |
| `crates/lightvm-mcp` | MCP stdio server spike |
| `crates/rfb-client` | Read-only RFB 3.8 client used by the UI |
| `spikes/` | F0 feasibility probes, kept for reference |
| `docs/` | Roadmap, status, architecture, security, ADRs, verification evidence |

## Building

Requirements: Rust 1.99.0 (pinned in `rust-toolchain.toml`). On Windows the MSVC linker needs the Visual Studio 2022 Build Tools environment loaded (`vcvars64.bat`) before running cargo.

```
cargo build --workspace --locked
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

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

Project documentation is written in Italian.

## How to read `docs/verification/`

Each file `F<phase>-<nn>-<topic>.md` documents one test: purpose, environment, procedure with exact commands, results and limits. Every item is marked `verificato` (verified), `fallito` (failed) or `non eseguito` (not run). Only a verification file can show that something works: a claim elsewhere without a reference to one does not count as evidence.

## License

Original code is released under the MIT License, see [`LICENSE`](LICENSE). QEMU, firmware, guest images and third-party dependencies carry their own licences, which the LightVM licence does not replace.

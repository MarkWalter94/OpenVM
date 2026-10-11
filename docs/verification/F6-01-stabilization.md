# F6-01 — Stabilization on the development host: database updates, full disk, host resume, crash recovery, SBOM, release artefact checks, regression script

- Execution date (UTC): 2026-10-11
- Executed by: development agent "F5/F6 backend" (Claude Code session); the desktop UI is owned by another agent (not modified here)
- Roadmap step: section 10 F6 ("Stabilizzazione pubblica"), the items that can be done on one Windows development host: "Regressione storage, sessioni, sicurezza e sospensione/ripresa dell'host"; "Recupero dopo crash, controlli su spazio esaurito e aggiornamento del database"; "Backup dei metadati prima della migrazione, rifiuto di downgrade incompatibili, aggiornamento solo con VM ferme e ripetizione dei probe dopo cambi QEMU/firmware"; "SBOM, inventario licenze"; "Verificare che l'artefatto pubblico non includa endpoint di test, driver di automazione UI incorporato, credenziali o impostazioni di debug". Sections 7.3, 8, 9.
- Related ADRs: ADR-0009, ADR-0010, ADR-0020, ADR-0023 (none modified; proposals at the end)
- Overall outcome: **partial** — items §1–§7 below **verified** with automated tests (fakes and injected faults where stated, real QEMU for the crash path through F5-03); the F6 exit criterion is **not met** and cannot be on this host: no code signing and no independent review (ADR-0020), one physical host only, no Linux, no real suspend of the host, release-artefact findings owned by the desktop and packaging (§6).

Allowed outcomes: **verified**, **failed**, **not executed** (section 17). Logs in `target\f6-evidence\` and `lab/f5/` (outside the repository).

## Environment

| Item | Value |
| --- | --- |
| Host | Intel Core i9-14900K, Windows 11 Pro 10.0.26300, WHPX (same host as F3-01…F5-01) |
| Toolchain | Rust 1.99.0 `x86_64-pc-windows-msvc`, wrapper `cargo-msvc.cmd`, `CARGO_TARGET_DIR=target\f6`, `CARGO_INCREMENTAL=0` |
| QEMU | 11.1.0 portable `lab/qemu/qemu-11.1.0/` (approved hashes, F0-02) |
| Tools not installed | `cargo-deny`, `cargo-cyclonedx`, `cargo-about` (no new tool installed: an offline licence check was written instead, §5) |

## 1. Database update safety

| Requirement (ROADMAP F6) | Implementation | Test | Outcome |
| --- | --- | --- | --- |
| Backup of the metadata before every migration | already in `vm-storage` (`migrate::backup_database`, `VACUUM INTO catalog.sqlite3.backup-v<N>-<UTC>`, called by `migrate_to` whenever a non-empty catalog needs a migration, before any write) | `vm-storage/tests/migrations.rs` `upgrade_from_v1_creates_dated_backup_and_keeps_data` (backup exists, is a consistent v1 database with the data), `upgrade_from_every_previous_version_matches_a_fresh_schema`, and the new open-runs test below (backup created once the refusal is lifted) | verified |
| Refusal of incompatible downgrades | already: a catalog whose schema is newer than the binary is refused (`StorageError::SchemaTooNew`, "downgrade not allowed"); the service exits with code 4 and that text on stderr | new `newer_schema_refusal_writes_nothing`: three refused opens leave the database file **byte-for-byte identical** (SHA-256), no backup file, `last_start` not written, data intact; `tampered_migration_is_detected` (an applied migration whose text differs is refused) | verified |
| Update only with the VMs stopped | **new**: `migrate_to` refuses a migration, before the backup and any write, while the catalog records runs without an end (`runs.ended_at IS NULL`: a VM of the previous version may still be running, for example with the `keep-vms` UI-loss policy, or crashed without reconciliation) — `StorageError::UpgradeWithOpenRuns {from, to, open_runs}`, message "start the previous version of LightVM, stop every VM and close it, then start this version (nothing was changed)". An up-to-date catalog with open runs opens normally (reconciliation is the service's job). | new `upgrade_is_refused_while_runs_of_the_previous_version_are_open` (v1 catalog with a running run: refusal, file unchanged, no backup, still v1; run closed → migration with backup) and `open_runs_do_not_block_a_catalog_without_migration` | verified |
| Repetition of the probes after a QEMU/firmware change | **new**: `qemu_backend::probe::{ProbeFingerprint, current_fingerprint, reprobe_if_changed}`; the service backend hashes again `qemu-system-x86_64`, `qemu-img` and the firmware code/variables template **before every VM start** (≈ 31 MB, tens of ms in release) and, if any changed since the probe, repeats the full probe (approved-hash check, capabilities, real accelerator probe) and refreshes the capabilities, machine and CPU model; an unapproved binary or a failed accelerator probe refuses the start with `unsupported` naming what changed. The probe also runs at every service start (unchanged). | new `probe_is_repeated_after_a_qemu_or_firmware_change` (fake distribution in a temporary folder: same files → no probe; firmware code replaced → one re-probe, change named; binary replaced by an unapproved one → the probe's `UnapprovedBinary` refusal is returned; firmware removed → "firmware presence" change) | verified with fakes; the re-probe with a real second QEMU build **not executed** (only one approved build exists) |

## 2. Disk-full handling

| Path | Implementation | Test (injection) | Outcome |
| --- | --- | --- | --- |
| Media import | already: free space checked on the copy's source size + 1 GiB margin before copying; an OS "disk full" error during the copy becomes `resource_limit` "not enough free space in the catalog for the medium" (journal intent aborted, temporary removed by the storage) | new unit test `adapters::catalog::tests::disk_full_is_a_clear_resource_limit` (injected `ERROR_DISK_FULL` 112 / `ENOSPC` 28 → `resource_limit`, no path in the message) | verified (mapping); a real full volume **not executed** (no spare volume; creating a VHD needs administrator rights) |
| Baseline | already: budget and free-space pre-check (estimate + 1 GiB); **new**: `qemu-img` failing with "No space left on device" / "There is not enough space on the disk" becomes `resource_limit` "not enough free space on the catalog volume (qemu-img …)" instead of an opaque I/O error (`image::reports_no_space`) | `qemu-backend` `image::tests::full_volume_is_a_resource_limit` (both QEMU texts); new service test `f3::baseline_on_a_full_volume_fails_cleanly`: the fake image tool writes part of the output and fails as `qemu-img` does on ENOSPC → `resource_limit` naming free space, **no baseline recorded, catalog `tmp` empty, nothing under `base/`, VM disk unchanged, VM still `stopped`, audit `baseline_create failed`**; with space back the same request succeeds | verified (injected) |
| VM start / run | **new**: a start is refused with `resource_limit` ("free some space first", details `free_mib`, `required_mib`) when the catalog volume has less than 2 GiB free (`START_FREE_MIN`), so the overlay, logs and reports keep room; during a run QEMU's default `werror=enospc` pauses the guest on a full disk (no write lost) | `adapters::backend::tests::start_needs_free_space_on_the_catalog_volume` (injected readings) | verified (check); a guest actually filling the volume **not executed**, and the paused-on-ENOSPC state is **not surfaced** to the user (open item) |
| Session reports (ROADMAP 8: "conservare una riserva per manifest ed esito minimo") | **new**: an 8 MiB reserve file `sessions/.reserve` is created with the first report of the catalog; if a report cannot be written the reserve is released and the write repeated; if it still fails a **minimal report** (header, termination, outcome, verdicts; events counted as `events_dropped`) is written; atomic replace keeps the previous file intact; a report that cannot be written at all stays in memory, dirty, and is retried at every flush (before, a terminated report was dropped from memory even when its write failed); screenshots fall back to their hash; the reserve is re-created when space is back (retry ≥ 60 s) | new `reports::tests` on a simulated volume that fails writes with the real OS "disk full" error: `full_volume_at_termination_uses_the_reserve` (reserve released, full terminated report on disk with `revoked`/`cancelled`, reserve re-created later), `full_volume_beyond_the_reserve_writes_a_minimal_report` (1 000+ events → minimal report with `grant_expired`/`timed_out`, no temporary left), `report_not_written_is_kept_and_retried` (nothing writable: kept in memory, screenshot not stored and no partial file, written by the next flush once space is back), `injected_error_is_the_disk_full_error` (Windows error 112 is `ErrorKind::StorageFull`) | verified (injected) |
| Catalog database (SQLite) | not changed: SQLite returns `SQLITE_FULL` and rolls the transaction back (WAL, `synchronous=FULL`); existing journal/rollback tests (`vm-storage` `failed_db_update_is_rolled_back`) | — | not executed with a real full volume |

## 3. Host suspend/resume (ROADMAP 7.3)

New module `lightvm-service/src/resume.rs` and supervisor hook:

- The supervisor thread never waits more than 1 s for a command. It marks the instant (wall clock and monotonic clock) before each wait and checks it as soon as the wait returns, **before serving the command and before the expiry check**. A gap above 5 s (`DEFAULT_RESUME_GAP`, configurable in `ServiceConfig::resume_gap`) by either clock means the process was frozen (sleep/hibernation) or the wall clock jumped forward: every agent session ends with **`host_resumed`**, its VMs are Emergency-Stopped, the gates and connections close (`publish_state_changes`), the reports are flushed (outcome `cancelled`). Resuming needs a new approval.
- Both clocks are read because a monotonic clock may or may not advance during a suspend depending on the platform; a backward wall-clock step is ignored. A forward clock change without a suspend is treated the same way (fail-safe). Suspends shorter than 5 s are within the lease rules (lease ≤ 10 s; the expiry check runs right after).
- No `WM_POWERBROADCAST`: it needs a window procedure and `unsafe` code, which the service forbids (`unsafe_code = "deny"`).
- The wall-clock source is injectable (`ServiceConfig::wall_clock`, `resume::WallClock`).

| Test | What it shows | Outcome |
| --- | --- | --- |
| `resume::tests::gaps_beyond_the_threshold_are_resumes` | 1 s wait → nothing; 600 s by the wall clock with a stopped monotonic clock → resume; 90 s by both → resume; wall clock 1 h back → nothing; exactly 5 s → nothing | verified |
| `f4::host_resume_terminates_the_sessions_and_stops_their_vms` (real IPC, real core, fake backend, **injected clock jump**) | a 2 s jump changes nothing; a 602 s jump: the agent's very next input request is not served (refused or connection closed), **no input forwarded after the resume**, VM stopped, session `terminated` with `host_resumed`, report `cancelled`, audit `session_terminate` with the cause; a new approval gives a working session | verified |
| Real host sleep/hibernation | — | **not executed** (the host is shared and cannot be suspended by the agent) |

## 4. Crash recovery (found while producing the F5-03 outcome states)

Two defects fixed:

1. A QEMU process that crashed or was killed from outside left the agent session **live** on a dead run (no termination, the report kept `pending`). Now `vm-core` ends the live sessions of the VM with **`vm_terminated`** on an unexpected exit (report: `infrastructure_error`); an orderly exit, a user/agent stop and the Emergency Stop keep their own causes. Tests: `vm-core/tests/observations.rs` `unexpected_exit_while_running_fails_and_discards_the_queue` (updated), new `orderly_exit_keeps_the_agent_session`; real QEMU: F5-03 case `outcome-infrastructure-error`.
2. A VM in `failed` (crash, failed start) **stayed `failed` forever** — no start, restore or deletion, also after a service restart (only `starting/running/stopping` entered reconciliation), although the UI says "the supervisor brings it back to Stopped". Now the service engine brings a published `failed` VM back to `stopped` through `recovering` at the next observation (≈ 1 s later; the run keeps its `crash`/`start_failed` end in the catalog, the audit has the state changes), and `load_catalog` reconciles a persisted `failed` VM at restart. Tests: `observations.rs` `load_catalog_never_trusts_a_persisted_process_state` and `reconcile_after_restart_returns_recovering_vms_to_stopped` (updated for `failed`); real QEMU: F5-03 (`vm_state_after` = `Stopped` after the kill, then restore and the final restore worked).

## 5. SBOM and licence inventory

New tool crate `crates/lightvm-sbom` (bin `lightvm-sbom`, not a product artifact; dependencies `serde_json` and `sha2`, already in the lockfile; no network):

- Input: `cargo metadata --format-version 1 --locked --offline --filter-platform <target>` and `Cargo.lock`. Output: **CycloneDX 1.5 JSON** of the packages linked into the shipped binaries (default roots `lightvm-desktop`, `lightvm-service`, `lightvm-mcp`; normal dependencies only; build- and dev-only dependencies left out; procedural macros listed with scope `excluded`), with `purl`, the declared SPDX expression (`A/B` normalised to `A OR B`), the **SHA-256 of each registry package from `Cargo.lock`**, the patched `egui-winit` marked as such, workspace crates marked as such, the C code declared (`libsqlite3-sys` with a nested `SQLite` component and its version read from the bundled `sqlite3.h`; `ring` with a property), optional `file` components with the SHA-256 and size of the built binaries (`--binary`), the dependency graph, a serial number derived from the inputs and a timestamp from `SOURCE_DATE_EPOCH` (reproducible output).
- `--notices FILE`: third-party notices with the licence/notice files of every shipped crate read from the local registry (including the font licences under `fonts/`), listing the crates that ship none.
- `--check-licences deny.toml`: offline licence check against the `[licenses]` allow list and per-crate exceptions of `deny.toml` (SPDX `AND`/`OR`/`WITH`/parentheses; malformed expressions fail). It evaluates the declared expressions only — `cargo-deny` (licence text detection, advisories, bans, sources) remains the reference check in CI.

| Result on the current lockfile | Value |
| --- | --- |
| Windows (`x86_64-pc-windows-msvc`) | 245 components: 234 registry packages with lockfile SHA-256, 10 workspace crates, 1 patched path crate (`egui-winit`); 18 procedural macros (`excluded`); SQLite 3.53.2 declared; licence check **passed** |
| Linux (`x86_64-unknown-linux-gnu`) / macOS (`aarch64-apple-darwin`) | 302 / 244 components; licence check **passed** |
| Crates shipping no licence file (Windows) | 21 (egui family, accesskit family, `rmcp`, `rmcp-macros`, `clipboard-win`, `spirv`, `profiling`, `enum-map`…): their SPDX licence applies; the packaging script already appends the standard MIT/Apache-2.0/BSL-1.0 texts for them in `THIRD-PARTY-LICENSES.txt` |
| Finding | the F0 spike `f0-console` (and through it `tempfile`, `uds_windows`) is still in the product graph of `lightvm-desktop` (ADR-0023 asks to remove it before F6) — owner: desktop |

Tests: `lightvm-sbom` unit tests (closure without build/dev-only crates, purl/licence/hash, proc-macro scope, patched crate, deterministic output, missing checksum and unknown root refused, RFC 3339 dates, SPDX policy evaluation and `deny.toml` parsing) and `tests/workspace.rs` (the real workspace: shipped crates present, `proptest`/`egui_kittest`/`lightvm-sbom` absent, every third-party component with a licence and a 64-hex hash, SQLite version found, licence check against the real `deny.toml`).

Packaging integration: **not edited** (owner: packaging). The exact change is in the section "Change for packaging/windows/build-package.ps1" below.

## 6. Release artefact checks

`scripts/check-release-artifacts.ps1 [-Path <dirs/files>] [-AllowHostPaths]` scans every `.exe`/`.dll` as bytes (ASCII/UTF-8 and UTF-16LE at both alignments) and the folder, exit 1 on findings:

- test-only environment hooks: `LIGHTVM_UPDATE_SCHEMAS`, `LIGHTVM_E2E_`, `LIGHTVM_F5_`, `LIGHTVM_LAB_DIR`, `LIGHTVM_QEMU_DIR`, `LIGHTVM_SERVICE_EXE`;
- UI automation driver (`egui_kittest`, `kittest`) and the service test fakes (`fake convert`, `fake overlay`, `lightvm-service-tests`, unit-test fixtures);
- debug/testing switches of the shipped programs (`--simulated-console`, `--smoke`, `--no-supervisor`) and debug-build markers (overflow-check panic messages);
- private keys and API-token shapes; the lab guest-credential file name;
- build-host paths (user profile, repository, cargo home) — findings unless `-AllowHostPaths`;
- files that must not be shipped (`lightvm-ipc-probe.exe`, `lightvm-peer-check.exe`, `lightvm-sbom.exe`, `f0-*.exe`, `*.pdb`).

Source review of the hooks (all crates and the desktop): `LIGHTVM_UPDATE_SCHEMAS` appears only in `lightvm-proto/tests/schema_snapshot.rs`; every `LIGHTVM_E2E_*`, `LIGHTVM_F5_*`, `LIGHTVM_QEMU_DIR`, `LIGHTVM_ISO`, `LIGHTVM_SERVICE_EXE`, `LIGHTVM_LAB_DIR` read is in `tests/` files; `egui_kittest` is a dev-dependency; the service `fakes`/`test_support` modules are `#[doc(hidden)]` library modules used only by tests (not referenced by any binary, so not linked — confirmed by the scan). The only `LIGHTVM_*` variable read by a shipped binary is `LIGHTVM_PAIRING_FILE` in `lightvm-mcp` (documented product configuration, folder privacy checked). Nothing in the crates owned here needed a new `cfg(test)` gate.

| Run | Result |
| --- | --- |
| Release build of `lightvm-desktop`, `lightvm-service`, `lightvm-mcp` with the **packaging flags** (`+crt-static`, `--remap-path-prefix` for cargo home/repository/target, `/PDBALTPATH:%_PDB%`), staged in a folder and scanned | `lightvm-service.exe` (9.8 MB) and `lightvm-mcp.exe` (7.9 MB): **no finding**, no host path. `lightvm-desktop.exe` (21.9 MB): **3 findings** — the switches `--simulated-console`, `--smoke`, `--no-supervisor` are in the shipped binary (usage text and parser of `apps/desktop/src/main.rs`); owner: desktop (gate them behind `cfg(debug_assertions)` or a non-default feature, or remove them from the product). No test hook, fake, kittest driver, key/token or host path in any of the three. |
| Same three binaries from a plain `cargo build --release` (no packaging flags; `regression.ps1 -Release`) | the same 3 desktop findings; build-host paths (`C:\Users\<user>`, cargo home, repository) as warnings in all three — they are removed only by the packaging flags, so the check must run on the staged package |
| Positive controls | a debug `lightvm-service.exe` → findings "debug build (overflow checks enabled)"; the `f4` test binary → findings `lightvm-service-tests`, `fake convert`, `fake overlay` (the test fakes) and the debug markers: the rules do detect what they look for |
| False positive removed | a bare `kittest` matched a comment in egui-wgpu's shader source embedded in the desktop binary; the rule is the crate name `egui_kittest` only |

Outcome: **failed** for `lightvm-desktop.exe` (3 debug switches, not owned here), **verified** for `lightvm-service.exe` and `lightvm-mcp.exe`.

## 7. Regression script

`scripts/regression.ps1 [-OutDir] [-TargetDir] [-Steps …] [-Release]` runs, each step with its log and a summary table (`summary.txt`), exit 1 if any step fails: `fmt` (`cargo fmt --all --check`), `clippy` (workspace, `-D warnings`), `clippy-ui-glow`, `clippy-ui-wgpu`, `clippy-client` (`lightvm-service --no-default-features`), `test` (`cargo test --workspace --locked --no-fail-fast`, totals parsed), `schema` (snapshot), `licences` (`cargo deny --locked check` when installed, else the offline `lightvm-sbom --check-licences` for the three targets of `deny.toml`), `sbom`, and with `-Release` the release build of the three binaries plus `check-release-artifacts.ps1`. The `#[ignore]` real-QEMU/real-client tests are not blocking suites (lab needed).

Run on 2026-10-11 06:28 UTC (`-NoDebugInfo -Release`, `CARGO_TARGET_DIR=target\f6reg`, working tree with this step's uncommitted changes and the desktop as left by the other agent at that time; logs in `target\f6-evidence\regression\`):

| Step | Result | Seconds | Detail |
| --- | --- | --- | --- |
| fmt | PASS | 1 | |
| clippy | PASS | 37 | 0 diagnostics |
| clippy-ui-glow | PASS | 31 | 0 diagnostics |
| clippy-ui-wgpu | PASS | 10 | 0 diagnostics |
| clippy-client | PASS | 11 | 0 diagnostics |
| test | PASS | 152 | **639 passed, 0 failed, 27 ignored** (whole workspace) |
| schema | PASS | 6 | 3 passed |
| licences | PASS | 3 | offline `lightvm-sbom` check, 3 targets (cargo-deny not installed) |
| sbom | PASS | 1 | 245 components |
| release-check | **FAIL** | 99 | 3 findings (desktop switches, §6) |

Before the workspace run, the crates changed here were tested on their own (`target\f6`, debug): **433 passed, 0 failed, 22 ignored** (`vm-storage`, `qemu-backend`, `vm-core`, `lightvm-proto`, `lightvm-service`, `lightvm-mcp`, `lightvm-sbom`); `cargo clippy --all-targets -D warnings` clean on the same crates and on `lightvm-service --no-default-features`; `cargo fmt` applied to them.

## Change for `packaging/windows/build-package.ps1` (for the packaging owner)

1. Ship the MCP adapter (F5, ADR-0021 D-4): add `'-p', 'lightvm-mcp', '--bin', 'lightvm-mcp'` to the `cargo build` arguments, `'lightvm-mcp.exe'` to the existence check loop, and `Copy-Item (Join-Path $Release 'lightvm-mcp.exe') $Staging` in the staging block (and `-p lightvm-mcp` in the `cargo tree` call of the licence section).
2. After the "Third-party licences" section, before "Build info and checksums" (so `SBOM.cdx.json` is listed in `SHA256SUMS.txt`):

```powershell
# --- SBOM (F6, ADR-0023) ------------------------------------------------------
Write-Step 'Generating SBOM.cdx.json (CycloneDX 1.5) and checking the licence policy'
$oldSde = $env:SOURCE_DATE_EPOCH
$env:SOURCE_DATE_EPOCH = [string][DateTimeOffset]::Parse((& git -C $Repo log -1 --format=%cI).Trim()).ToUnixTimeSeconds()
try {
    Invoke-Cargo -CargoArgs @('run', '--release', '--locked', '--offline', '-q', '-p', 'lightvm-sbom', '--',
        '--workspace', $Repo, '--target', $Triple, '--product-version', $Version,
        '--binary', (Join-Path $Staging 'lightvm-desktop.exe'),
        '--binary', (Join-Path $Staging 'lightvm-service.exe'),
        '--binary', (Join-Path $Staging 'lightvm-mcp.exe'),
        '--out', (Join-Path $Staging 'SBOM.cdx.json'),
        '--check-licences', (Join-Path $Repo 'deny.toml'))
} finally { $env:SOURCE_DATE_EPOCH = $oldSde }

# --- Release artefact check (F6) ------------------------------------------------
Write-Step 'Checking the staged artefacts (test hooks, debug settings, host paths, credentials)'
& powershell -NoProfile -ExecutionPolicy Bypass -File (Join-Path $Repo 'scripts\check-release-artifacts.ps1') -Path $Staging
if ($LASTEXITCODE -ne 0) { throw 'release artefact check failed (see the FINDING lines)' }
```

3. In `BUILDINFO.txt` add a line `SBOM:         SBOM.cdx.json (CycloneDX 1.5, crates/lightvm-sbom)`.

## What remains for the F6 exit criterion

- **Code signing and independent review**: unavailable (ADR-0020, P-6, I-5) — the public release stays blocked.
- **Two physical hosts** of the certified target (Intel/AMD if both declared): one host only.
- **Linux**: not executed (R2a).
- Real host suspend/resume, a real full volume during import/baseline/run, a second approved QEMU build for the re-probe: not executed (fakes and injected faults only).
- Run-time ENOSPC: QEMU pauses the guest; LightVM does not yet report the paused-on-full-disk state to the user.
- Release-artefact findings owned by others (§6): debug switches of `lightvm-desktop` and the `f0-console` spike in its graph; packaging changes above.
- Installer final, uninstall preserving data, documentation and visible limitations, vulnerability management process: outside this work.

## Proposals for the ADRs (not edited here)

- ADR-0023: SBOM tool decided — in-repo `lightvm-sbom` (CycloneDX 1.5, offline, from `cargo metadata` + `Cargo.lock`), plus the offline licence check as a fallback when `cargo-deny` is not installed.
- ADR-0009 / SECURITY: host resume ends the agent sessions by clock-gap detection in the supervisor loop (no power broadcast, no `unsafe`); a QEMU crash ends the sessions with `vm_terminated`.
- F1-02 / ARCHITECTURE: migrations refused while runs of the previous version are open; `failed` VMs reconciled by the supervisor.

# F1-03 — `qemu-backend` with real QEMU: probe, rejections, NIC, 20 start/stop cycles and service disk

- Execution date (UTC): 2026-10-10, from about 09:00 to 09:15
- Executed by: development agent (Claude Code session), at the request of the project owner
- Roadmap step: section 10 F1 (exit criterion: "disk creation, at least 20 real start/stop cycles without orphan processes, clear error with the accelerator missing, no unexpected NIC, rejection of invalid paths and resources"); backlog `docs/RELEASE_PLAN.md` F1-03, F1-05, F1-06, F1-07, F1-08, F1-12
- Related ADRs: ADR-0002, ADR-0004, ADR-0007, ADR-0009, ADR-0010, ADR-0011, ADR-0012 (not modified)
- Overall outcome: **partial** — the four real tests of `qemu-backend` and the end-to-end test of the service with the primary disk are **verified on hardware** on a single Windows host; the VMs stay in the UEFI firmware (no operating system started); **they do not count as the F1 exit criterion until F0 is closed** (section 10)

Allowed outcomes: **verified**, **failed**, **not executed** (section 17). Raw logs in `lab/logs/` (outside the repository): `f1-03-real-all.txt`, `f1-03-probe-run.txt`, `f1-04-e2e-disco.txt`, `f1-03/s1791623264863-cicli.log`, `f1-03/s1791623264863-cicli.csv`, `f1-03/s1791623264863-screendump.png`, `f1-03/s1791623326125-probe.log`, `f1-03/s1791623327921-rifiuti.log`, `f1-03/s1791623329287-nic.log`, `qemu-extract.txt`.

## Purpose

Demonstrate with real QEMU and WHPX, through the `qemu-backend` APIs used by the supervisor:

1. probe of the approved binaries (hashes), rejection of a non-approved hash, clear error with the accelerator missing and no TCG fallback;
2. rejection of invalid paths, media, resources, CPU models, machines and protocols before creating any process;
3. detection of an unexpected NIC with `info network` and `query-pci`;
4. at least 20 real start/stop cycles with a disk created by `qemu-img`, per-run overlay, Job Object, private QMP, no orphan processes, no NIC, reconciled state; real screendump and input on the first cycle; Emergency Stop on the last;
5. creation of the primary disk by the service (`lightvm-service`) at the first start of a VM, closing the `TODO(F1-12)`.

It does not demonstrate: boot of an operating system (the VM stays in the firmware or in the ISO boot loader), a successful ACPI shutdown, installation to disk (F1-17), Linux, release performance, behaviour under load, a second user.

## Environment

| Item | Value |
| --- | --- |
| Host | Intel Core i9-14900K (32 logical processors), 64 GB RAM (65 303 MiB detected), system SSD with about 62 GiB free |
| OS and build | Microsoft Windows 11 Pro 10.0.26200 |
| Accelerator and state | WHPX: `probe_accelerator whpx: Available { machine: "pc-q35-11.1", cpu: "qemu64" }` |
| Toolchain | Rust 1.99.0 `x86_64-pc-windows-msvc` (rustup 1.29.1, installed in this session; `rustup-init.exe` with SHA-256 `6f4bef66…bdb7e` equal to the published `.sha256` file); MSVC 14.44.35207 from Visual Studio Community 2022 with the component `Microsoft.VisualStudio.Component.VC.Tools.x86.x64` added in this session (see Limits) |
| QEMU binary | 11.1.0 (`v11.1.0-12130-ge470268ff4`) extracted portably with 7-Zip from `https://qemu.weilnetz.de/w64/qemu-w64-setup-20260811.exe`: installer SHA-512 equal to the published `.sha512` file and to F0-02 (`5bcf9eed…16bf543`), SHA-256 `f98a8aeb…e2a59f` equal to F0-02; `qemu-system-x86_64.exe` SHA-256 `47d57a6072e0bb3bd98f87926eb129eb1736dfe818c67b3b81ef7ce4edd0b3cd`, `qemu-img.exe` SHA-256 `9d930582d0d574bb106d89982c547688e10b8f356192d302cc3d0ae4e8674534` (equal to F0-02); 3388 files, 1 256 078 336 bytes |
| Firmware | `share/edk2-x86_64-code.fd` SHA-256 `33090cc0…6dbb2a`, `share/edk2-i386-vars.fd` SHA-256 `5d2ac383…be5d1e` (equal to F0-02) |
| Guest / media | `ubuntu-26.04.1-desktop-amd64.iso` downloaded from `https://releases.ubuntu.com/26.04.1/`, SHA-256 `601e30fbf5d97759367c632e2c33630665039b7e2158fd068403da3ccf1bda1f` (`sha256sum -c`: OK; GPG signature of the checksum file not re-verified in this session, see `F0-03`) |
| Graphics session | local (the tests open no windows) |
| Code | branch `f1-consolidamento` starting from `3958b2e`, with this session's changes to the service (primary disk, measured free space) |
| Other conditions | non-dedicated host: the times are not release measurements (section 13) |

## Procedure

MSVC environment loaded with `vcvars64.bat` of Visual Studio 2022 Community in a `cmd` (wrapper in the session scratchpad, not in the repository).

```text
set LIGHTVM_QEMU_DIR=<workspace>/lab/qemu/qemu-11.1.0
cargo test -p qemu-backend --test real_qemu -- --ignored --nocapture --test-threads=1

set LIGHTVM_E2E_QEMU_DIR=<workspace>/lab/qemu/qemu-11.1.0
cargo test -p lightvm-service --test core_path -- --ignored --nocapture
```

`LIGHTVM_F1_CYCLES` not set: 20 cycles (default value). The configuration of each cycle is that of the test `reale_cicli_start_stop` (`crates/qemu-backend/tests/real_qemu.rs`): `pc-q35-11.1`, WHPX, `qemu64`, 2 vCPUs, 2048 MiB, per-run qcow2 overlay on a 1024 MiB base created with `prepare::create_disk`, ISO as installation media, per-run copy of the UEFI variables.

## Results

### `qemu-backend` tests (4 tests, 64.6 s, `test result: ok. 4 passed`)

| Item | Outcome | Evidence |
| --- | --- | --- |
| Disk creation with `prepare::create_disk` and refusal to overwrite an existing disk | verified | `disco qcow2 da 1024 MiB creato … in 34.0905ms (197120 byte)`; second creation → error (test assertion) |
| Probe of the approved binaries and of the accelerator | verified | `QEMU 11.1.0 … sha256 47d57a60…; acceleratori ["tcg", "whpx"]; macchina consigliata pc-q35-11.1`; `probe_accelerator whpx: Available` in 150.7 ms |
| Non-approved hash rejected | verified | `unsupported / il binario qemu-system-x86_64 non corrisponde a una build QEMU approvata … avvio rifiutato` |
| Accelerator missing: clear error, no TCG fallback | verified | `-accel kvm` on the Windows binary → `unsupported / acceleratore hardware non disponibile: KVM non è incluso nel binario QEMU. Avvio rifiutato: nessun ripiego sull'emulazione TCG …`; accelerator different from the probe → same code with hints about HypervisorPlatform |
| Rejection of invalid paths and resources | verified | 18 rejections, `nessun processo QEMU creato`: comma, relative, UNC, device, `..`, protocol, media with comma, unregistered media → `invalid_argument`; 0 and 65 vCPUs, 64 MiB and excessive RAM → `resource_limit`; `-cpu host` and missing model → `unsupported`; CPU with flags and alias `q35` → `invalid_argument`; `nbd`, `https` → `invalid_argument` (offline profile) |
| Unexpected NIC detector | verified | VM started on purpose with `virtio-net-pci`: `info network` and `query-pci` (network class) report it, `check_no_nic` → `infrastructure_error` |
| 20 real start/stop cycles | verified | see below |

### Start/stop cycles (CSV `s1791623264863-cicli.csv`, SHA-256 `cc7f88b8…885fb8`)

| Item | Outcome | Evidence |
| --- | --- | --- |
| Start with WHPX, a single process in the Job Object, `running` state reconciled | verified (20/20) | `run.accelerator().enabled == "whpx"`, `active_processes() == 1`, `reconcile` → `Running` |
| No NIC | verified (20/20) | `verify_no_nic` succeeded at every cycle (column `nic_ok = true`) |
| Orphaned QMP socket removed by the backend | verified | fake `qmp.sock` file created before cycle 1, start succeeded |
| Real screendump in a private directory, PNG, no leftover `.ppm` | verified | `screendump dopo 1.09s: 1280x800, PNG 40681 byte, sha256 1f9aced2…2598, non nero 1.79%`: OVMF (TianoCore) with `BdsDxe: starting Boot0001 "UEFI QEMU DVD-ROM"` |
| Real input and key release; rejection of stale geometry, superseded `run_id`, `power` key; screenshot size limit | verified | `input: movimento + shift premuto, rilasciati 1 tasti`; negative assertions of the test; `screenshot_limited(16)` → `resource_limit` |
| Stop with `quit` only (odd cycles 1–19) | verified (10/10) | phase `Quit`, code 0, class `Clean`, cause `QmpQuit`; duration min 91.6 ms, median 102.1 ms, max 322.2 ms |
| Stop with escalation `system_powerdown` (2 s) → `quit` (even cycles 2–18) | verified (9/9) | the firmware does not respond to ACPI (note `nessuna uscita dopo system_powerdown`, as D-7 in F0), then `quit` succeeded; duration min 2098.8 ms, median 2106.3 ms, max 2110.8 ms |
| Emergency Stop (cycle 20) | verified | phase `Kill`, code 58853, class `EmergencyStop`, duration 41.3 ms |
| After every stop: 0 processes in the job, Job Object closed, handle released, QMP socket removed, state `stopped` | verified (20/20) | columns `attivi_dopo = 0`, `job_chiuso = true`, `handle_rilasciato = true`, `stato_riconciliato = Stopped`; `qmp_socket_left = false` (assertion) |
| No orphan processes | verified | `identita_dopo = NotFound` for all 20 PIDs (PID + creation time identity); final check on 20 identities |

Times (20 samples, non-dedicated host): overlay min 86.0 / median 96.1 / p95 104.6 / max 111.1 ms; start until QMP ready min 153.2 / median 157.2 / p95 162.9 / max 164.3 ms; run closing median 0.2 ms. No errors, no timeouts, no retries.

### Service: primary disk and free space (`core_path::e2e_real_qemu_create_start_status_stop`, 6.65 s)

| Item | Outcome | Evidence |
| --- | --- | --- |
| Catalog free space measured (no longer a fixed value) | verified | `HostCapacity { … free_storage_gib: 62, reserved_storage_gib: 16, accelerator_available: true … }` |
| Primary disk created by the service at the first start (`vms/<vm_id>/primary.qcow2`, `vm-storage` layout) | verified | `disco principale: …\vms\01a12514-…\primary.qcow2 (197120 byte)` after the first stop |
| Second start with the same disk, Emergency Stop | verified | start succeeded; `EmergencyStopReport { processes_terminated: 1, completed: true }` in 22.8 ms |
| IPC → core → SQLite → real backend → audit | verified | start 180.3 ms, stop 3.13 s (3 s powerdown not completed, then `quit`), 20 audit rows with `vm_start`, `vm_stop`, `emergency_stop` `succeeded` |

### Workspace build and tests on the same host

| Item | Outcome | Evidence |
| --- | --- | --- |
| `cargo build --workspace --locked` | verified | after adding the MSVC x64 component (before: `LNK1104 msvcrt.lib`, as in F0-01) |
| `cargo fmt --all --check` | verified | no differences |
| `cargo clippy --workspace --all-targets --locked -- -D warnings`; `lightvm-desktop` with `glow` only and `wgpu` only; `lightvm-service --no-default-features` | verified | no warnings |
| `cargo test --workspace --locked --no-fail-fast` | verified | 328 passed, 0 failed, 7 ignored (tests with real QEMU) in 55 test binaries |

## Limits

- A single host, a single QEMU build, a single guest; no extension to other hosts (section 5).
- The VMs do not boot an operating system: they stay in OVMF or in the ISO boot loader. The ACPI shutdown never completed (escalation to `quit`); a real guest may behave differently.
- The `qemu-backend` tests use the backend APIs directly; through the service only the end-to-end test (2 starts) was run, not the 20 cycles.
- The service's primary disk is created outside the catalog journal: a pre-existing file (even a partial one after a crash) is used as is. Journal and atomic rename are in F3 (`RELEASE_PLAN.md`, F3 open points).
- The MSVC x64 component was added to the user's Visual Studio 2022 Community installation (with UAC confirmation): the previous installation contained only the `onecore` libraries, without headers and without `vcvarsall.bat`. Rust is installed with `--no-modify-path`: `cargo` is not in the system PATH.
- GPG signature of the Ubuntu checksum file not re-verified in this session (verified in `F0-03` for the same ISO and the same hash).
- **Validity for the exit criterion:** F0 is not closed (P-1, P-4…P-7 in `docs/STATUS.md`); per section 10 these tests do not close F1.

## Required actions

1. Close F0 (user actions U-3, U-5, U-7 in `docs/STATUS.md`), then repeat this test as evidence for the F1 exit criterion.
2. Repeat the 20 cycles through the service (IPC → core → backend), not only on the backend APIs.
3. F1-17: richer CPU model and boot to the desktop and from disk.
4. Move the creation of the primary disk into the catalog journal (F3).

## Proposed decisions

No new decision: the results confirm ADR-0007 (no TCG fallback), ADR-0009 (Emergency Stop with OS primitives), ADR-0012 (private QMP) on the new host.

## Update of 2026-10-10 (from about 09:25 to 09:35 UTC): cycles through the service and boot with a named CPU model

Section appended at the end (rule 8 of `docs/verification/README.md`): the previous sections remain as recorded. Same host, toolchain, QEMU, firmware and ISO.

### Procedure

```text
set LIGHTVM_E2E_QEMU_DIR=<workspace>/lab/qemu/qemu-11.1.0
cargo test -p lightvm-service --test core_path -- --ignored --nocapture e2e_real_qemu_cicli

set LIGHTVM_QEMU_DIR=<workspace>/lab/qemu/qemu-11.1.0
cargo test -p qemu-backend --test real_qemu -- --ignored --nocapture reale_boot_live
```

- `core_path::e2e_real_qemu_cicli_attraverso_il_servizio` (new): IPC → core → SQLite → `qemu-backend` → QEMU, one VM (1 vCPU, 1024 MiB, 8 GiB disk created at the first start, no ISO) started and stopped 20 times; odd cycles `force_stop`, even `stop` (service policy under test: `system_powerdown` 1 s → `quit` 10 s → kill 10 s), cycle 20 Emergency Stop; after every stop, `qemu-system-x86_64.exe` processes that are children of the test process counted with `Win32_Process`. Log: `lab/logs/f1-12-cicli-servizio.txt`.
- `real_qemu::reale_boot_live_cpu_nominale` (new): `-cpu Skylake-Client-v4`, 2 vCPUs, 4096 MiB, live ISO, overlay on a 1024 MiB base, one screendump every 10 s for 240 s, then Emergency Stop. Logs: `lab/logs/f1-17-boot-skylake.txt`, `lab/logs/f1-03/s1791624568749-boot-cpu.log` and 24 PNGs.

### Results

| Item | Outcome | Evidence |
| --- | --- | --- |
| 20 start/stop cycles **through the service**, one QEMU process per start, no leftovers after every stop and after the service shuts down | verified | `ciclo i: … residui 0` for i = 1…20; `test result: ok. 1 passed` in 47.7 s |
| Reconciled state and audit | verified | `running` after every start and `stopped` after every stop (via `vm_status`); audit `succeeded`: `vm_start` 20, `vm_stop` 19 (includes `force_stop`), `emergency_stop` 1 |
| Primary disk reused across cycles | verified | `vms/<vm_id>/primary.qcow2` present at the end of the test; created only once |
| Times | recorded | start (IPC → `running`) min 139.3 / median 154.1 / p95 1181.1 / max 1820.9 ms; `force_stop` (10) median 55.9, max 1149.3 ms; `stop` (9) median 1122.1, max 3101.7 ms; Emergency Stop 49.6 ms. Cycles 7–11 are slower (up to 1.8 s to start): cause not investigated, non-dedicated host; no errors or timeouts |
| F1-17: boot with `Skylake-Client-v4` to the desktop | verified | process alive for 245 s; firmware and boot loader until t≈80 s, at t=101 s GNOME live desktop with the "Welcome to Ubuntu" window (screendump SHA-256 `a1c329f9…0b96`, 99.93 % non-black); deviation D-1 overcome with a named model richer than `qemu64` (`-cpu host` remains rejected) |
| F1-17: installation of Ubuntu to disk and boot from the disk | not executed | requires driving the installer (or an `autoinstall` from the boot loader); not attempted in this session |

### Limits

- A single boot sample with `Skylake-Client-v4`; `Haswell-v4` not tested.
- The 20 cycles through the service use a reduced shutdown policy (`system_powerdown` 1 s) set by the test; the service's default policy (60 s) was not used in the cycles.
- Validity for the exit criterion: unchanged from the previous sections (F0 open).

## Update of 2026-10-10 (from about 11:25 to 11:40 UTC): installation to disk, boot from the disk and ACPI shutdown (F1-17)

Section appended at the end (rule 8 of `docs/verification/README.md`). Same host, QEMU and ISO; `-cpu Skylake-Client-v4`.

### Procedure

1. Autoinstall configuration in `lab/vms/f1-17/user-data` (test user `lvm`, random password kept only in `lab/`, SHA-512 hash in the file; `storage.layout: direct`; `shutdown: poweroff`; `interactive-sections: []`), `CIDATA` volume generated with `pycdlib` (`lab/media/f1-17-cidata.iso`, SHA-256 `85145f96…a476f1`) on the second CD-ROM.
2. `cargo test -p qemu-backend --test real_qemu -- --ignored --nocapture reale_f1_17_installazione`: 32 GiB qcow2 disk and persistent UEFI variables in `lab/vms/f1-17/`, 4 vCPUs, 6144 MiB, Ubuntu ISO + `CIDATA`. The desktop installer reads the autoinstall but asks for confirmation in the summary: the test clicks "Install" via QMP at 140 s (first attempt without the click stopped by hand at 2 minutes, no write to the disk).
3. `cargo test … reale_f1_17_boot_da_disco_e_arresto_acpi`: boot from the disk without ISO (4096 MiB), screenshot every 15 s for 120 s, then ordinary shutdown with `system_powerdown` (90 s wait, then `quit` and kill).

### Results

| Item | Outcome | Evidence |
| --- | --- | --- |
| Autoinstall read by the desktop installer | verified | screenshot of the "Review your choices" summary with identity, layout, `shutdown: poweroff` |
| Installation to disk completed and the guest powering itself off | verified | `esito dopo 482.18s: Some((GuestShutdown, Some(0)))`; disk `disco.qcow2` 6 915 817 472 bytes |
| **Boot from the disk** up to the login screen | verified | at 30 s GDM screen with the user "LightVM prova" (`…-f1-17-boot-030s.png`), stable until 120 s |
| **Ordinary ACPI shutdown of a real guest** | verified | `fase Powerdown, causa GuestShutdown, codice Some(0), durata 3.55s`; no orphans |

With this section F1-17 is complete on the first host: named CPU richer than `qemu64`, installation to disk, boot from the disk. The disk installed in `lab/vms/f1-17/` is the reference guest for measurements with a real guest (CPU ceiling under load, orderly shutdowns, F3 fixture).

### Limits

- One sample; the click on "Install" depends on the position of the button at 1280×800 (Ubuntu 26.04.1 installer).
- No access to the guest graphical session was tested in this section.

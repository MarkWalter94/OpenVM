# R6-01 — Import of VMware VMs by conversion: parser, conversion, journal, IPC and UI

- Execution date (UTC): 2026-10-14
- Executed by: development agent on the project host
- Roadmap step: section 12, R6 (import of VMware VMs), in parallel by owner decision
- Related ADRs: ADR-0030 (import by conversion), ADR-0027 (guest profiles), ADR-0017 (storage budget), ADR-0029 (files of the user, guard share modes), ADR-0025
- Overall outcome: partial (parser, hostile inputs, mapping, command lines, journal, conversion with the real `qemu-img`, IPC and UI verified in test; real VMware VMs not executed)

## Purpose

ADR-0030 lists the evidence for the import: parser tests on every VMDK create type produced by `qemu-img` and on hostile `.vmx` files and descriptors, conversion with interruption, recovery, cancellation and budget refusal, and real VMware VMs started to the desktop. This file records what was run for the first two and states that the third was not executed.

What this file does NOT demonstrate: that any real VMware VM (Windows with PVSCSI, LSI Logic or SATA; Linux) boots in LightVM after the import; that the `pvscsi`, `mptsas1068` and `vmware-svga` devices work with a guest driver under WHPX; anything about the offline storage driver enablement for LSI Logic Windows guests (not implemented). No VMware VM of the host was read: every VMware folder was written by the tests.

## Environment

| Item | Value |
| --- | --- |
| Host (model, CPU, RAM, storage) | Intel Core i7-12800HX, 31.6 GiB RAM; worktree on volume E:, test folders under the user's temporary folder (C:) |
| OS and build | Windows 11 Pro 10.0.26200 |
| Accelerator and state | WHPX (only for the device start check of §Results 9) |
| Toolchain / relevant versions | Rust toolchain of `rust-toolchain.toml`, MSVC; `cargo test --locked` with a private target directory |
| QEMU binary (version, origin, SHA-256) | QEMU 11.1.0, `lab/qemu/qemu-11.1.0` (ADR-0010): `qemu-img.exe` SHA-256 `9d930582d0d574bb106d89982c547688e10b8f356192d302cc3d0ae4e8674534`, `qemu-io.exe` SHA-256 `2b4bce81239a3982d3e274a1409251a8c67b26876ff07e5ce921017e2d42da9a`, `qemu-system-x86_64.exe` as in `R2c-01` |
| Firmware (file, version, SHA-256) | SeaBIOS `share/bios-256k.bin` of the same build (hash in `R2c-01`), only for §Results 9 |
| Guest / media (name, version, SHA-256) | none: VMDK fixtures made at test time with `qemu-img create -f vmdk` (`-o subformat=...`, `-b ... -F vmdk`) and filled with `qemu-io` |
| Graphics session (local, RDP, headless) | local; kittest snapshots with the wgpu renderer |
| Other conditions (antivirus, group policies, etc.) | as in `F0-02` |

## Procedure

The tests that use `qemu-img` read the QEMU folder from `LIGHTVM_QEMU_DIR` and print a message and pass without checking anything when it is not set; here it was set:

```powershell
$env:LIGHTVM_QEMU_DIR = 'E:\OpenVM\lab\qemu\qemu-11.1.0'
cargo test --locked --package vmware-import
cargo test --locked --package qemu-backend --lib --test vmdk_convert
cargo test --locked --package vm-storage --test vm_import --test migrations
cargo test --locked --package vm-core --test vm_import
cargo test --locked --package lightvm-service --test vmware_import --test f4
cargo test --locked --package lightvm-proto
cargo test --locked --package lightvm-desktop --test vmware_import_ui --test visual_snapshots
```

Device start check (§Results 9): `qemu-system-x86_64.exe -nodefaults -accel whpx -cpu Skylake-Client-v4 -smp cpus=4,sockets=2,cores=2,threads=1 -m size=256M` with SeaBIOS, an empty 64 MiB QCOW2 disk as `-blockdev` and, in turn, (a) `pc-q35-11.1` + `pvscsi` + `scsi-hd` + `vmware-svga`, (b) `pc-q35-11.1` + `mptsas1068` + `scsi-hd` + `VGA`, (c) `pc-i440fx-11.1` + `ide-hd` on `ide.0`; each process observed for 6 s, then terminated.

## Results

| # | Item | Outcome | Evidence |
| --- | --- | --- | --- |
| 1 | Every create type of `qemu-img` (monolithicSparse, monolithicFlat, twoGbMaxExtentSparse and twoGbMaxExtentFlat with two extents, streamOptimized): examined, files of the chain = files `qemu-img info` opens, virtual size, plan (Windows 10 UEFI, NVMe, 2 cores per socket); `qemu-img info` still reads the chain while the guard holds it | verified | `vmware-import` `tests/fixtures.rs::every_create_type_of_qemu_img` |
| 2 | Snapshot chain of three levels named by file name as VMware writes them: chain followed, cross-checked (3 images, 128 MiB), plan (LSI Logic → AHCI with the preparation flag and warning, memory raised to the Vista minimum, not-imported list); `qemu-img` output naming an unvalidated file or with fewer images refused; parent recreated after the snapshot (CID mismatch) refused | verified | `tests/fixtures.rs::snapshot_chain_is_followed_and_cross_checked`, `::parent_changed_after_snapshot_is_refused` |
| 3 | Hostile disk paths in the `.vmx` and in descriptors (extents and `parentFileNameHint`): `..` (also staying inside), absolute outside the folder, UNC, `\\?\`, `\\.\PhysicalDrive0`, `\\.\GLOBALROOT\...`, device names, drive-relative, alternate data streams; absolute inside the folder accepted; messages without folders | verified | `tests/hostile.rs::disk_paths_outside_the_folder_are_refused`, `::extent_and_parent_paths_follow_the_same_rules`, `::absolute_path_inside_the_folder_is_accepted` |
| 4 | Links: a junction inside the VM folder on the way to a disk, the `.vmx` reached through a junction (refused: not where the user saw it); a file symlink (run only if the host allows creating it) | verified (junctions); file symlink as allowed by the host | `tests/hostile.rs::links_and_junctions_are_refused` |
| 5 | Oversized `.vmx` (> 1 MiB), large text file as descriptor, embedded descriptor too large, descriptor offset past the end; encrypted (`encryption.*`), suspended (`checkpoint.vmState`, `.vmss`), locked (`.vmx.lck`, `.vmdk.lck`), `cleanShutdown = "FALSE"`, unknown guest type; looping chain, chain of 33, CID mismatch, extent that is a disk of the chain; `fullDevice`, RDM, `COWD`; `.vmx` path refusals; no disk; disk choice by `bios.hddOrder` with other disks and CD listed | verified | `tests/hostile.rs` (11 tests) |
| 6 | Parsers on random bytes and single-byte mutations (512 cases each): no panic, accepted output within the limits, only known keys kept | verified | `tests/fuzz.rs` |
| 7 | Mapping rules: guest types, firmware rules, controllers, suggested names | verified | `vmware-import` unit tests (`plan`, `vmx`, `vmdk`, `paths`) |
| 8 | Command lines: `pvscsi`/`mptsas1068` + `scsi-hd`, PIIX IDE on i440fx, NVMe, AHCI, `vmware-svga`, `-smp cpus=4,sockets=2,cores=2,threads=1`, audit of the arguments, devices emitted = devices required; refused combinations; the existing profiles' exact argument lists unchanged | verified | `qemu-backend` `cmdline::tests::imported_hardware_argv`, `::profile_rows_exact_argv` |
| 9 | The devices of (a), (b), (c) accepted by QEMU 11.1.0 under WHPX with SeaBIOS: the process runs for 6 s without error (stderr: only the usual `arat` CPUID warnings and `Ignoring request for interrupt vector 0`) | verified (device creation only; no guest, no boot from these controllers observed) | §Procedure |
| 10 | Conversion with the real `qemu-img`: chain with data in every level converted (`convert -p -f vmdk -O qcow2`), same guest-visible content as the source (`qemu-img compare`), no backing file, `check` clean, virtual size kept, source files byte-identical before and after, progress reaching 100, existing destination never overwritten, cancellation, no output files left | verified | `qemu-backend` `tests/vmdk_convert.rs` |
| 11 | Journal `import_vm` (schema 9): disk and VM row published together; cancellation; crash during the conversion and crash after publishing without the row rolled back when the catalog is reopened (temporary, disk and VM folder removed); name conflict at the commit rolled back; existing row or destination refuse the import; migration 8 → 9 with backup, rows, identifiers and sequence kept | verified (crash simulated by dropping the catalog without committing) | `vm-storage` `tests/vm_import.rs`, `tests/migrations.rs` |
| 12 | Core: user only (agent denied and audited), creation checks plus imported hardware, name unique also against an import in progress and released on abort, Vista x64 refused on Windows hosts, Linux BIOS allowed for imports only, imported hardware fixed after the import, storage failures audited | verified | `vm-core` `tests/vm_import.rs` |
| 13 | Through the IPC with the real `qemu-img`: preview (names, guest, topology, controller, snapshot levels, not imported) writes nothing; import of a snapshot chain with VMware SVGA; VM `stopped` with its converted disk, content equal to the source, VMware files unchanged, `tmp/` empty, `vm_import` succeeded; progress events (checking, converting 100, finishing); same name refused | verified | `lightvm-service` `tests/vmware_import.rs::preview_and_import_of_a_snapshot_chain` |
| 14 | Refusals through the IPC: locked VM (reason `locked`, no folder in the message), LSI Logic preview with the warning, storage budget (1 MiB free), bad paths; nothing left, refusals audited | verified | `tests/vmware_import.rs::refusals_leave_nothing_and_are_audited` |
| 15 | Cancellation through the IPC during the conversion: by the user (`cancel_vmware_import`), a second import refused while one runs, by the Emergency Stop, by the service shutdown; nothing left and the catalog reopens clean | verified | `tests/vmware_import.rs::cancellation_by_the_user_and_by_the_emergency_stop` |
| 16 | Agent sessions refused for the three VMware requests (role matrix) | verified | `lightvm-service` `tests/f4.rs::request_role_matrix` |
| 17 | UI: entry and picker, preview with the LSI Logic warning and the display choice, Linux/PVSCSI preview, invalid or taken name, not enough space, refused preview, progress, cancel, failure, success; snapshots | verified (kittest) | `lightvm-desktop` `tests/vmware_import_ui.rs`, `tests/visual_snapshots.rs` |
| 18 | A crash of the supervisor process during a real conversion | not executed (the journal recovery is verified at the storage level, item 11) | — |
| 19 | Real VMware VMs (Windows with PVSCSI, LSI Logic, SATA; Linux) imported and started to the desktop | partial — one Linux VM to the session entry, one refused VM, one NVMe/BIOS boot-failure finding; Vista and Mint not executed (`R6-01b`) | `docs/verification/R6-01b-vmware-import-real.md` |
| 20 | Offline storage driver enablement for LSI Logic Windows guests | not executed (not implemented; manual preparation documented) | ADR-0030 |

## Limits

- Fixtures are made by `qemu-img`, not by VMware: VMware-specific descriptor variants (absolute `parentFileNameHint` written by older VMware versions, `windows-1252` descriptors with non-ASCII names, `ddb.*` keys of newer hardware versions) were covered only as synthetic text where relevant; a non-UTF-8 descriptor is refused.
- The guard share mode (`FILE_SHARE_READ` only) is a Windows mechanism; other hosts are not covered (R2a).
- The device check of item 9 shows that QEMU creates the devices under WHPX; it does not show that SeaBIOS or OVMF boot from them or that a guest driver works.
- A VMware disk whose size is not a whole number of GiB is recorded rounded up: the backend grows the converted disk to that size at the first start (less than 1 GiB of unallocated space at the end of the disk).

## Required actions

- Import and start real VMware VMs as listed in ADR-0030 (item 19), recording each guest, controller and outcome in a new dated section or file — started in `R6-01b` (partial: one Linux VM to the session entry, one refused VM, one NVMe/BIOS boot-failure finding; Windows Vista and Mint outstanding).
- Resolve the NVMe/BIOS combination found in `R6-01b` item 5: refuse or warn at the preview when the imported firmware cannot boot the imported disk controller (legacy BIOS + NVMe), with a `vmware-import` plan test.
- Spike for the offline storage driver enablement of LSI Logic Windows guests (ADR-0030), then its implementation behind the existing hook (`lightvm_service::vmware::prepare_storage_driver`).

## Proposed decisions

None: ADR-0030 records the implementation and the evidence still missing.

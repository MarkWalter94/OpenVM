# ADR-0030 — Import of VMware virtual machines by conversion

- Status: Accepted (2026-10-14): implemented and verified in test (`R6-01`); real VMware VMs and the offline storage driver enablement pending
- Date: 2026-10-14
- Owner: project owner (decision); implementation recorded below

## Context

Users already run their test guests in VMware Workstation or Player (for example a Windows Vista VM with Visual Studio 2010, `.vmx` + `.vmdk` with a snapshot chain, LSI Logic SCSI disk). Recreating such a VM from an ISO means reinstalling the guest and its software. The project owner decided that LightVM imports VMware VMs, **by conversion**: the VMware disk is converted into a managed QCOW2 disk and the VMware VM is never modified.

Facts on the first host: VMware Player 25 runs on the Windows Hypervisor Platform (log: `Monitor Mode: ULM`, `Hyper-V detected`), the same layer as WHPX. The bundled QEMU 11.1.0 provides `qemu-img` with the `vmdk` driver and the devices `pvscsi`, `mptsas1068`, `lsi53c895a`, `megasas`, `ich9-ahci`, `vmware-svga`, `vmxnet3`, `usb-host`, `usb-redir`. It has no emulation of the LSI53C1030 parallel SCSI controller that VMware calls `lsilogic`.

## Decision

**Input.** The user selects a `.vmx` file in the app (file picker). The VM must be powered off: a `.vmx.lck` lock, a suspended state (`checkpoint.vmState`, `.vmss`) or a non-clean shutdown refuses the import with a message telling the user to shut the VM down in VMware. Encrypted VMs (`encryption.*`) are refused.

**Parsing (untrusted input).** The `.vmx` and the VMDK descriptors are parsed as untrusted text: size limit, `key = "value"` lines only, known keys only, everything else ignored and nothing passed to QEMU. Disk extents and snapshot parents (`parentFileNameHint`) must resolve to regular files inside the VM folder: absolute paths outside it, `..`, links, reparse points, devices and remote paths are refused; the chain depth is bounded. `qemu-img info --output=json` checks format and virtual size against the storage budget before anything is written.

**Mapping.**

| VMware | LightVM |
| --- | --- |
| `guestOS` | guest profile (ADR-0027); an unknown value refuses the import |
| `numvcpus`, `cpuid.coresPerSocket`, `memsize` | vCPU topology and RAM, within the host limits |
| `firmware = "efi"` or absent (BIOS) | OVMF with a new variable store, or SeaBIOS |
| `scsiN.virtualDev = "pvscsi"` | `pvscsi` (the guest keeps its VMware PVSCSI driver) |
| `scsiN.virtualDev = "lsisas1068"` | `mptsas1068` |
| `scsiN.virtualDev = "lsilogic"` | storage driver enablement (below), then the profile controller |
| `sataN` | AHCI |
| `ideN` | IDE |
| `nvmeN` | NVMe |
| current disk state (base + snapshot chain) | one QCOW2 disk, chain flattened |
| CD with an ISO | offered as a data medium (ADR-0029), only on user confirmation |
| `svga.*` with VMware Tools in the guest | `vmware-svga` offered as display device |

Intermediate snapshots, the suspended state, the network, sound, 3D and VMware Tools features are not imported; shared folders, USB devices and clipboard are recreated through LightVM's own integration (ADR-0031), not from the `.vmx`.

**LSI Logic (`lsilogic`) guests.** Windows boots only from a controller whose driver is enabled for boot (otherwise stop 0x7B). Since QEMU has no LSI53C1030, LightVM enables the inbox storage driver of the target controller in the **converted copy** before the first start (offline edit of the guest `SYSTEM` hive: service start type and critical device entry for the controller's PCI identifier). The VMware disk is never written. The method (hive editing on the converted image, the privileges it needs on the host, the Windows versions it covers) is chosen by a spike before implementation; until then an `lsilogic` Windows guest is imported with a clear warning and documented manual preparation in VMware.

**Conversion.** `qemu-img convert -f vmdk -O qcow2` runs in the supervisor's process tree with the parsing limits above, into a staging area of the catalog, journaled (`import_vm` intent), with progress, cancellation and recovery after a crash; the VM row is written only when the disk is complete. The resulting disk is an ordinary LightVM disk (baselines, overlays, budget).

## Implementation (2026-10-14)

- **Parser** (`crates/vmware-import`, no process, nothing written): `.vmx` up to 1 MiB, 50 000 lines, `key = "value"` lines only, known keys only (VMware `|XX` escapes decoded, control characters refused); descriptors up to 64 KiB, read from a text file or from the `KDMV` header of a hosted sparse extent (monolithicSparse, streamOptimized, snapshot deltas), at most 4 096 extents; create types monolithicSparse, monolithicFlat, twoGbMaxExtentSparse/Flat, streamOptimized, vmfs, vmfsSparse; extent types `FLAT`, `SPARSE`, `ZERO`, `VMFS`, `VMFSSPARSE`; raw device mappings, `fullDevice`/`partitionedDevice`, `SESPARSE`, ESX `COWD` and custom types refused. Every extent and `parentFileNameHint` is resolved against the folder of the file that names it and must be a regular file strictly inside the VM folder, without `.`/`..`, UNC, device or drive-relative forms, alternate data streams, device names, and without a link or reparse point on any component; the chain is at most 32 descriptors, with equal sizes and consistent CIDs. **Choice for the embedded descriptors:** they are parsed here (the bounded `KDMV` header gives the descriptor's offset and size) rather than left to `qemu-img info`, because `qemu-img` opens every extent and parent a descriptor names before its output can be checked: a hostile `\\server\share` or `\\.\PhysicalDrive0` would already be opened. `qemu-img info --backing-chain --output=json` runs only on the validated chain and is cross-checked (one `vmdk` image per descriptor, same files, every file it opens among the validated ones, same virtual size).
- **Refusals before any conversion:** `.lck` entries in the folder (locked), `.vmss` file or non-empty `checkpoint.vmState` (suspended), `cleanShutdown = "FALSE"`, any `encryption.*` key, unknown guest type, Windows 11, 32-bit Linux, EFI with Windows XP/Vista/7 or a 32-bit guest, unknown SCSI `virtualDev`. The `.vmx` path must be the location the system resolves (no link, junction, mount point, network or substituted drive). Messages name files, never folders.
- **Mapping** (`vmware_import::plan`): guest types `winXP*`, `winVista[-64]`, `windows7[-64]`, `windows8[-64]` (8.1, with a warning: VMware uses the same type for 8 and 8.1), `windows9`/`windows10[-64]`, and 64-bit Linux types (Other Linux); vCPUs and RAM clamped to the host limits and the guest minimum with warnings; `cpuid.coresPerSocket` kept as `-smp cpus=N,sockets=S,cores=C,threads=1` when it divides the vCPUs; disk size rounded up to whole GiB. Controllers: SATA → AHCI (q35), IDE → PIIX IDE (the i440fx board, which has that controller), NVMe → `nvme`, PVSCSI → `pvscsi` + `scsi-hd`, LSI SAS → `mptsas1068` + `scsi-hd`; LSI Logic and BusLogic → the profile's controller (AHCI for Vista and later, IDE for XP, virtio for Linux). Linux VMs may keep legacy BIOS (`GuestOs::import_firmware`; created VMs stay UEFI only). The hardware lives in `VmConfig::imported` and cannot change after the import; the existing profiles' command lines are unchanged (`DeviceProfile::for_imported`).
- **LSI Logic Windows guests:** imported with the warning *Windows may not start: storage driver preparation needed* and `storage_driver_preparation` set; manual preparation documented in `USER_GUIDE.md` §6.3; the hook `lightvm_service::vmware::prepare_storage_driver` is called on the converted copy and currently changes nothing (the spike decides the method).
- **Conversion** (`lightvm_service::vmware`, supervisor): examination on a worker; the chain is then held with `FILE_SHARE_READ` only (Windows) and examined again, so the files converted are the files checked; core checks and storage budget (free space of the catalog volume minus the host reserve must hold the chain's data, at most the virtual size, plus metadata); intent `import_vm` (schema 9); `qemu-img convert -p -f vmdk -O qcow2` from the approved build in its own Job Object into the journal temporary, progress published as events, cancellation by the user, the Emergency Stop, the revocation or closing of the connection and the shutdown; `qemu-img check` and the virtual size of the result; the disk published as `vms/<vm_id>/primary.qcow2` and the VM row written in one commit; reconciliation removes an interrupted import. One import at a time; audit `vm_import`.
- **Not implemented here:** the automatic offline driver enablement; offering the ISO of a VMware CD drive as a data medium (the CD is listed under "not imported" with the hint to import the ISO from the media library); a content comparison of the converted disk with the source after every import (`qemu-img convert` exit status, `check` and the virtual size are relied upon; the tests compare the content).

## Alternatives considered

- Use the VMDK in place behind a QCOW2 overlay: rejected by the owner (the LightVM VM would break as soon as the VMware VM changes).
- Patch QEMU to emulate LSI53C1030: rejected (LightVM uses upstream QEMU builds).
- Require VMware Tools features in QEMU (backdoor RPC): QEMU's `vmport` implements only a few commands; not usable.

## Required evidence

Recorded in `docs/verification/R6-01-vmware-import.md`.

- Parser tests: every VMDK create type produced by `qemu-img` (monolithicSparse, monolithicFlat, twoGbMaxExtentSparse, twoGbMaxExtentFlat, streamOptimized), snapshot chains, hostile descriptors and `.vmx` files (traversal, links, absolute paths, oversized, encrypted, suspended, locked). **Verified in simulation/test** (`vmware-import` `tests/fixtures.rs` with fixtures made at test time by the bundled `qemu-img`, `tests/hostile.rs`, `tests/fuzz.rs`); the symbolic-link case runs only where the host allows file symlinks (junctions always).
- Conversion: interruption and recovery, cancellation, budget refusal. **Verified in simulation/test** (`vm-storage` `tests/vm_import.rs` for the journal and the reopen after a crash; `qemu-backend` `tests/vmdk_convert.rs` and `lightvm-service` `tests/vmware_import.rs` with the real `qemu-img` for the conversion, the content compared with the source, cancellation by the user, the Emergency Stop and the shutdown, and the budget refusal). A crash of the supervisor process itself during a real conversion was not run (simulated at the storage level).
- Real VMware VMs, at least: Windows with PVSCSI, Windows with LSI Logic, Windows with SATA, Linux; first start in LightVM to the desktop. **Not verified.** The `pvscsi`, `mptsas1068` and `vmware-svga` devices have not been started under WHPX by the product either.
- Offline storage driver enablement for LSI Logic Windows guests: **not implemented** (spike pending).

## References

Roadmap, sections 7.4, 8, 12 (R4b, R6); ADR-0017 (storage budget), ADR-0027 (guest profiles), ADR-0029 (media in place), ADR-0031 (host integration).

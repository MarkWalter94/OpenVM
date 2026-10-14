# ADR-0030 — Import of VMware virtual machines by conversion

- Status: Accepted (2026-10-14): direction set; evidence pending
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

## Alternatives considered

- Use the VMDK in place behind a QCOW2 overlay: rejected by the owner (the LightVM VM would break as soon as the VMware VM changes).
- Patch QEMU to emulate LSI53C1030: rejected (LightVM uses upstream QEMU builds).
- Require VMware Tools features in QEMU (backdoor RPC): QEMU's `vmport` implements only a few commands; not usable.

## Required evidence

- Parser tests: every VMDK create type produced by `qemu-img` (monolithicSparse, monolithicFlat, twoGbMaxExtentSparse, twoGbMaxExtentFlat, streamOptimized), snapshot chains, hostile descriptors and `.vmx` files (traversal, links, absolute paths, oversized, encrypted, suspended, locked).
- Conversion: interruption and recovery, cancellation, budget refusal.
- Real VMware VMs, at least: Windows with PVSCSI, Windows with LSI Logic, Windows with SATA, Linux; first start in LightVM to the desktop. **Not verified.**

## References

Roadmap, sections 7.4, 8, 12 (R4b, R6); ADR-0017 (storage budget), ADR-0027 (guest profiles), ADR-0029 (media in place), ADR-0031 (host integration).

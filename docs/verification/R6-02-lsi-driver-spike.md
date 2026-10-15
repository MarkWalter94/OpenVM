# R6-02 — Spike: offline storage-driver preparation for LSI Logic Windows guests

- Execution date (UTC): 2026-10-14
- Executed by: development agent on the project host
- Roadmap step: section 12, R6, required action 2 of `R6-01` (offline storage driver enablement for LSI Logic Windows guests)
- Related ADRs: ADR-0030 (import by conversion, `StorageDriverPreparation` warning), ADR-0027 (guest profiles)
- Status: IN PROGRESS — lab-level spike to decide the technique before implementing it in the product pipeline

## Purpose

ADR-0030 maps a VMware LSI Logic disk to the LightVM target controller (AHCI on q35 for Windows 8.1/10/11, IDE on i440fx for XP/older) and flags the imported VM with a `StorageDriverPreparation` warning: the Windows guest has no driver for the new controller and fails with `INACCESSIBLE_BOOT_DEVICE` until the storage driver is enabled for the target controller. This spike decides, with executed evidence on the project host, which offline technique makes the imported disk bootable without manual work inside the guest:

1. **DISM offline enablement via a VHD copy** (`qemu-img convert` to VHD → `DISM /Image /Enable-Driver` → back to QCOW2), driver source from an approved Windows ISO in the lab media.
2. **Registry-hive edit from a preparation Linux VM** (boot the converted disk in a disposable QEMU with ntfs-3g + a hive editor) — recorded as the fallback if DISM is not usable (e.g. elevation not available).

The spike is lab-level: it verifies the technique on a converted copy of a real LSI Logic guest disk, outside the product pipeline. Product integration (journaling, elevation, cancellation, where the step sits in `lightvm_service::vmware::prepare_storage_driver`) is the follow-up design decision.

What this file does NOT demonstrate: that the technique is safe for every LSI Logic Windows guest; that the product pipeline implements it (not implemented); anything about PVSCSI or `mptsas1068` guests (they keep a working QEMU device, no preparation needed).

## Environment

| Item | Value |
| --- | --- |
| Host | Intel Core i7-12800HX, 31.6 GiB RAM; Windows 11 Pro 10.0.26200 |
| QEMU | 11.1.0, `lab/qemu/qemu-11.1.0` (ADR-0010), same binaries and hashes as `R6-01` |
| Guest disk | converted copy of the Windows Vista x86 guest of `D:\CerantolaVM` (LSI Logic, two-level snapshot chain, 40 GiB base + 11 GiB delta), kept in `lab/vms/lsi-spike/` |
| Driver source | `lab/media/Win10_22H2_Italian_x32v1.iso` (x86, matches the 32-bit guest) |
| Elevation | offline DISM and VHD mount require administrator; the shell used by the agent is **not** elevated — the elevated step is a prepared script (`lab/scripts/lsi-spike-dism.ps1`) run by the user |

## Procedure

1. Convert the Vista chain (chain top `Windows Vista-cl1-000001.vmdk`,
   two levels) to a QCOW2 copy and to a dynamic VHD in
   `lab/vms/lsi-spike/`, with the approved `qemu-img` (read-only on the
   VMware folder):
   `qemu-img convert -p -f vmdk -O qcow2 "D:\CerantolaVM\Windows Vista-cl1-000001.vmdk" E:\OpenVM\lab\vms\lsi-spike\vista.qcow2`
   then `qemu-img convert -p -f qcow2 -O vhd E:\OpenVM\lab\vms\lsi-spike\vista.qcow2 E:\OpenVM\lab\vms\lsi-spike\vista.vhd`.
2. Record the image's current storage drivers and enable the
   target-controller chain from the Win10 x86 ISO driver store
   (`lab/scripts/lsi-spike-dism.ps1`, elevated: `dism /Image /Get-Drivers`
   before, `dism /Image /Enable-Driver /Driver:<iso>\drivers /Recurse`,
   `dism /Image /Get-Drivers` after).
3. Boot the prepared disk with the imported hardware of the Vista profile
   (pc-q35, SeaBIOS, Skylake-Client-v4, AHCI `ide-hd`) under WHPX and
   screendump until a desktop or a stop screen.
4. Repeat step 3 with the unprepared copy as the negative control.

## Results

| # | Item | Outcome | Evidence |
| --- | --- | --- | --- |
| 1 | Unprepared copy boots to `INACCESSIBLE_BOOT_DEVICE` (negative control) | — | — |
| 2 | Prepared copy reaches the desktop on the AHCI controller | — | — |
| 3 | Driver set before/after in the image | — | `dism.log` |

## Limits

- One guest (Vista x86) is used; the same INF chain (storahci/pciide/atapi from the x86 store) is expected to cover Win7–Win11 x86/x64, but each version is not executed here.
- The technique costs two full disk conversions and a temporary VHD copy of the whole disk; the budget check of the product pipeline must include it.
- Offline DISM requires administrator on the host; the constraint and its handling in the product is recorded in the decision below.

## Proposed decision

(pending: adopt the DISM-via-VHD technique in `prepare_storage_driver` with an explicit admin requirement, or keep manual preparation with the warning)

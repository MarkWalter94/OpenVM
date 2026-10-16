# R6-02 — Offline storage-driver preparation for LSI Logic Windows guests: the storage helper

- Execution date (UTC): 2026-10-15 (supersedes the 2026-10-14 spike skeleton)
- Executed by: development agent on the project host
- Roadmap step: section 12, R6, required action 2 of `R6-01` (offline storage driver enablement for LSI Logic Windows guests)
- Related ADRs: ADR-0030 (import by conversion), ADR-0027 (guest profiles)
- Status: **DECIDED, IMPLEMENTED AND VERIFIED** — the method is the one recorded below; it is wired into the import pipeline and executed on the real Windows Vista guest of `D:\CerantolaVM` (boot A/B: stop 0x7B unprepared, desktop prepared)

## Purpose

ADR-0030 maps a VMware LSI Logic disk (which QEMU does not emulate) to the LightVM target controller — SATA AHCI on q35 for Windows Vista and later, IDE for XP — and the guest must have the driver of that controller enabled for boot or it stops with `INACCESSIBLE_BOOT_DEVICE` (stop 0x7B). This file records the decided technique, with executed evidence on the project host: which offline edit makes the imported disk bootable without manual work in the guest.

The original 2026-10-14 skeleton proposed two candidates: (1) offline DISM on a VHD copy (needs an approved Windows ISO and an elevated shell on the host); (2) a registry-hive edit from a Linux preparation VM. **Candidate 2 is adopted, refined into the storage helper**; candidate 1 is dropped.

What this file does NOT demonstrate: that every LSI Logic Windows guest is bootable after the preparation (the preparation needs the driver to be installed in the guest, and it refuses a hibernated or unclean disk — see §Limits); anything about PVSCSI or `mptsas1068` guests (they keep a working QEMU device, no preparation needed).

## Decision: the storage helper

A tiny Linux **helper appliance** (a separate, approved image — not part of LightVM) is booted headless by the approved `qemu-system-x86_64` with one disk attached: the **converted copy** of the imported Windows guest (a journal temporary; the VMware files are never written). The appliance mounts the Windows volume with ntfs-3g and edits the offline `SYSTEM` hive with chntpw `reged`, then unmounts and powers off. The supervisor (QEMU) then reads back the keys and verifies the edit before the VM is published.

Why this and not DISM:

- **No elevation.** The host shell does not need administrator rights: the helper is a guest of the already-approved QEMU, editing a file in the catalog (candidate 1 needed an elevated `dism`, which the service does not have and the user should not be asked for).
- **No driver is injected.** Only the guest's **own** inbox driver of the target controller is set to start at boot (`Start`=0) — nothing is copied from a Windows ISO, so there is no driver-store version drift between 32/64-bit or between editions, and no ISO dependency on the host.
- **No VHD round trip.** The edit happens in place on the QCOW2 the import already produced: no second full-disk conversion, no temporary VHD copy (candidate 1 doubled the conversion cost and the budget).
- **Refusals are first-class.** A hibernated or Fast Startup volume, a volume not shut down cleanly, a missing driver, a dirty hive: the helper reports a stable code and the import stops with the user action to take (see §Protocol, §Pipeline integration).

Privileges on the host: none beyond what LightVM already has (the approved QEMU, the catalog directory). The helper runs on the host reference machine (q35) with the nominal CPU model and SeaBIOS, 256 MiB, one vCPU, no network, no display; QEMU runs in its own Job Object, so cancellation, the Emergency Stop, the revocation and the death of the supervisor terminate it.

## The appliance

Built reproducibly by `packaging/storage-helper/build.py` from `packages.lock` (Alpine Linux 3.24 x86_64 binary packages pinned by SHA-256) and `init` (the `/init` script): sorted cpio entries, fixed metadata, mtime 0, gzip without name or time — two independent builds in the lab produced byte-identical output (both under `lab/storage-helper/`, hashes equal).

| File | Size | Contents |
| --- | --- | --- |
| `lightvm-storage-helper.vmlinuz` | 12 637 184 bytes | Alpine `linux-virt` 6.18.56-r0 kernel, unchanged |
| `lightvm-storage-helper.initrd` | 1 454 378 bytes | `init`, busybox-static 1.37.0-r31, musl 1.2.6-r2, ntfs-3g 2026.2.25-r0 (+libs), chntpw 140201-r6 `reged`, three kernel modules (`virtio_blk`, `fuse`, `qemu_fw_cfg`) |

SHA-256 (approved in `qemu_backend::storage_helper::APPROVED_HELPER_SHA256`, extensible by development approvals of kind `storage-helper`): kernel `dc99c86b…915c72`, initrd `4efcd0ba…e7cf2` (full values in the committed constant and in `SHA256SUMS`).

Licences (recorded in the `PACKAGES` file next to the images, with origin and aports commit): linux-virt GPL-2.0-only, busybox-static GPL-2.0-only, ntfs-3g and ntfs-3g-libs GPL-2.0-only, chntpw GPL-2.0-only AND LGPL-2.1-only, musl MIT. **The appliance is therefore a separate GPL image**, distributed and approved separately from the LightVM binaries: the service looks for it in a folder of the installation (`--storage-helper-dir`, default under the user state directory) and uses it only when both files carry an approved SHA-256; without it, imports keep the warning and the manual preparation (the behaviour of `R6-01`).

## The edit: what the preparation changes

Only the offline `SYSTEM` hive of the converted copy, per control set (current and default, from `Select`):

| Guest | Controller (QEMU device, PCI id) | Edit |
| --- | --- | --- |
| Windows Vista, 7 | AHCI (`ich9-ahci`, 8086:2922, class 010601) | `msahci` `Start`=0; critical device `PCI#VEN_8086&DEV_2922&CC_010601` → `msahci` |
| Windows 8.1, 10 | AHCI (same) | `storahci` `Start`=0 and every `StartOverride` value 0 (no critical device database since Windows 8) |
| Windows XP | PIIX3 IDE (8086:7010, i440fx) | `intelide`, `pciide`, `atapi` `Start`=0; critical devices `PCI#VEN_8086&DEV_7010` → `intelide`, `Primary_IDE_Channel`/`Secondary_IDE_Channel` → `atapi` |

No other key is touched. Windows 11 is refused at the import (ADR-0027), so it needs no plan; 32-bit guests keep the same plans (the driver files are the guest's own).

## Protocol

Two helper runs, the job in two `fw_cfg` files, the result on the serial console (records always behind the `LIGHTVM-PREP ` prefix, so exported hive text cannot forge one):

1. **Inspection (read-only).** The volume is mounted read-only; the service keys of the plan are exported. The supervisor requires each driver to exist as a kernel driver (`Type`=1) — a key without it is `NotInstalled`, and the import stops naming the driver. (A read-only mount is also the safety property: nothing is written before the checks pass.)
2. **Edit + export.** The supervisor builds the `.reg` of §The edit (`CurrentControlSet` replaced by the helper with the guest's control sets); the helper mounts the volume read-write with `norecover` — an NTFS volume not cleanly unmounted is **refused, not reset** — imports it with `reged` (a report without `SUCCEEDED` is a failure) and exports every key it touched. The supervisor parses the exports as untrusted text (bounded, only the plan's values looked at) and requires: every service `Start`=0 and `Type`=1, every `StartOverride` 0, every critical device entry naming the right `Service` and `ClassGUID`. Any mismatch is `NotVerified` and the import stops (the disk is rolled back with the journal).

Refusals the helper reports, and the import's message: `hibernated` (exit 14 of ntfs-3g, or a read-only fallback for a hibernated volume — ntfs-3g exits 0 and falls back to read-only, which the helper detects in `/proc/mounts` and refuses) → *start the VM in VMware, shut it down completely (Shift + Shut down, or `shutdown /s /t 0`)*; `unclean` (exit 15, or the read-only fallback for an unclean volume) → *shut the guest down from Windows*; `no_windows` / `several_windows` → unsupported, with the manual path; `hive_dirty` (the hive sequence numbers differ before or after the edit) → the SYSTEM hive was not written back cleanly. The volume is unmounted and checked clean before the VM powers off.

## Pipeline integration

`lightvm_service::vmware::prepare_storage_driver` runs the two runs above on the **converted copy**, in the journal temporary, as the step after `qemu-img convert` and `check` and before publication: the phase `PreparingDisk` is published to the UI (progress), the runs are cancellable (the user, the Emergency Stop, the shutdown), the disk is checked again after the edit, and the row is written with `storage_driver_preparation` cleared when the driver is enabled (the VM's warning then disappears). The preview flips the `StorageDriverPreparation` warning into `StorageDriverEnabled` when the preparation will run; otherwise the old warning and the manual preparation (`docs/USER_GUIDE.md` §6.3) stay.

## Evidence (Windows Vista x86, `D:\CerantolaVM`)

| # | Item | Outcome | Evidence |
| --- | --- | --- | --- |
| 1 | Conversion of the two-level LSI Logic chain (40 GiB virtual): source folder hash-compared before/after, converted content compared with the source | source unchanged; `qemu-img compare`: *Images are identical* | `lab/vms/lsi-prep/vista-source-{before,after}.sha256`, `vista-log.txt` |
| 2 | Inspection run: Windows found on `/dev/vda1`, one control set (`ControlSet001`), hive clean (sequence 30041/30041), `msahci` present as a kernel driver with `Start`=4 | as expected (`Type`=1, `Start`=4) | `lab/vms/lsi-prep/ahci-inspect-serial.txt` |
| 3 | Edit run: the plan's `.reg` imported (1 key changed, 1 new key added), `reged` *operation SUCCEEDED*, exported keys show `msahci` `Start`=0 and the critical device entry `Service`=`msahci`, `ClassGUID`=`{4D36E96A-…}`; hive clean afterwards; `RESULT ok` | verified by read-back | `lab/vms/lsi-prep/ahci-prep-serial.txt` |
| 4 | Negative control, product hardware (pc-q35-11.1, WHPX, Skylake-Client-v4, 2 GiB, disk on the built-in AHCI `bus=ide.2`, unprepared copy, `AutoReboot`=0 so the screen is stable): stops at **STOP 0x0000007B (0x8039EBB0, 0x00000034)** and stays there | boot failure as predicted | `lab/logs/lsi-prep/ahci-neg-0204s.png` (+ the earlier IDE run, `vista-unprepared-noreboot-0043s.png`) |
| 5 | Positive control, same hardware, the prepared copy (only difference: item 3): **reaches the desktop** (Windows Vista Business, build 6002, with its activation dialog) in ≈ 200 s | boot to desktop | `lab/logs/lsi-prep/ahci-prep-0206s.png`, `ahci-prep-0238s.png` (the boot series) |
| 6 | Refusal of an unclean volume: a booted-then-killed copy is mounted read-only by ntfs-3g and the helper refuses the edit (`RESULT error unclean`), the volume untouched | refusal works | `lab/vms/lsi-prep/probe2-serial.txt` |
| 7 | Reproducible build: two runs of `build.py` from the same `packages.lock` and `init` give identical `vmlinuz` and `initrd` (SHA-256 equal, matching the approved constants) | verified | `lab/storage-helper/out`, `lab/storage-helper/out-repro` |

Observation (not the product path): the prepared copy also boots from the PIIX3 IDE of q35 (`lab/logs/lsi-prep/vista-prepared-0083s.png`, desktop), while the unprepared copy failed there too (recovery menu, then stop 0x7B). The Vista import of the product targets AHCI; the IDE result is recorded because the lab exercised it first (an earlier script mapped "AHCI" to `ide.0` by mistake, corrected in `lab/scripts/guest-boot.ps1`), and it shows the disk is not tied to the AHCI edit.

## Limits

- One guest (Vista x86) executed end to end; the Win7/8.1/10 plans are the same mechanism (`storahci` for 8.1/10, no critical device database) and the XP plan the same keys as the documented manual preparation, but not executed here. The product applies the preparation only where a plan exists and the inspection verifies the driver is installed; every other combination keeps the warning and the manual path.
- The `hibernated` refusal path is coded and unit-tested with the helper's serial format, but no hibernated disk was executed in the lab (the refusal is triggered by ntfs-3g exit 14 or its read-only fallback; the `unclean` fallback was executed, item 6).
- The helper runs with WHPX on the host reference machine; a host without a usable accelerator (or with a non-q35 reference machine) does not run the helper and keeps the manual path.
- The preparation touches only the `SYSTEM` hive: it does not repair a broken boot loader or a damaged disk (a disk that does not boot in VMware does not boot after the import, prepared or not).

## Proposed decision

Adopted: the storage helper of §Decision, as implemented in `packaging/storage-helper`, `qemu_backend::storage_helper` and `vmware_import::driver_prep`, wired into the import pipeline behind the existing hook. The 2026-10-14 DISM-via-VHD candidate is dropped (elevation, ISO dependency, doubled conversion cost).

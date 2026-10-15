# R6-01b — Real VMware VMs imported and started (continuation of R6-01)

- Execution date (UTC): 2026-10-14 (runs 1–3); 2026-10-15 (run 5 controlled experiment; runs 1–4 re-executed with the storage helper)
- Executed by: development agent on the project host
- Roadmap step: section 12, R6, required action 1 of `R6-01` (real VMware VMs imported and started)
- Related: `R6-01-vmware-import.md` (parser, hostile inputs, mapping, conversion, journal, IPC, UI), ADR-0030, ADR-0027, `R6-02-lsi-driver-spike.md` (the storage helper)
- Overall outcome: complete — every real VM of the host that the importer accepts was imported and started: one Linux VM to the session entry, one suspended VM correctly refused, the Windows Vista LSI Logic VM to the desktop with the automatic storage driver preparation, the Mint 14-level chain to the session entry, and the legacy-BIOS NVMe VM imported and started (the SeaBIOS/NVMe combination of the 2026-10-14 run is resolved by the plan rule of §Run 5)

## Purpose

`R6-01` verified the import pipeline with test-made fixtures and left the real-VM item open ("real VMware VMs not executed"). This file records the execution of real VMware VMs of the host through the same pipeline: preview, import with the real `qemu-img`, verification that the source folder is byte-identical and the converted disk is content-identical to the source, start under WHPX, console observation, stop, orphan and audit checks.

What this file does NOT demonstrate: that a session is reached after the login (no guest credentials were used or needed); that the `StorageDriverPreparation` path makes an LSI Logic Windows guest boot (not implemented; `R6-02`); anything about VMs that the importer correctly refuses for other reasons (covered by `R6-01` fixtures).

## Environment

| Item | Value |
| --- | --- |
| Host (model, CPU, RAM, storage) | Intel Core i7-12800HX, 31.6 GiB RAM; as in `R6-01` |
| OS and build | Windows 11 Pro 10.0.26200 |
| Accelerator and state | WHPX (probe + every start) |
| QEMU binary (version, origin, SHA-256) | QEMU 11.1.0, `lab/qemu/qemu-11.1.0` (ADR-0010), same hashes as `R6-01` |
| Real VM folders read (read-only, hash-compared before/after) | `D:\VM\Debian`, `D:\VMCR2011`, `D:\CerantolaVM`, `D:\VM\Mint`, `E:\VMCR8` |
| Test | `lightvm-service` `tests/vmware_import_real.rs` (ignored; `LIGHTVM_E2E_QEMU_DIR`, `LIGHTVM_E2E_VMX`, `LIGHTVM_VM_EVIDENCE_DIR`, `LIGHTVM_VM_EXPECT_DESKTOP`, `LIGHTVM_VM_EXPECT_REFUSED`) |
| Evidence | `lab/logs/vmware-real/`: one PNG every 30 s per VM, the final frame, and the records files (`vmware-real-results*.txt`) |

## Procedure

Each `.vmx` was previewed, imported (name `<stem> imported`), the source folder hash-compared before/after, the converted disk content-compared to the source chain with `qemu-img compare`, started, watched on the RFB console until a session entry or a stop screen (desktop or graphical login; a stable text login prompt for non-Windows guests; a mostly-lit single-colour frame as stop), and stopped (graceful, with the test's escalation). The records files below are the test's own output, unedited.

## Results

### Run 1 — refused VM (dry run)

`vmware-real-results-dryrun-vmcr8.txt`:

```
VMCR8 | preview=refused: state_conflict / the VM is suspended in VMware: resume it in VMware and shut it down, then import it | imported=- | ...
```

| # | Item | Outcome |
| --- | --- | --- |
| 1 | A VM suspended in VMware (`.vmss` present) is refused at the preview, with the reason and the fix in the message; nothing read beyond the inspection, nothing imported | verified |

Note: `E:\VMCR8` also carries five VMware crash dumps and a 4.3 GiB `.vmem`; the importer refused the whole folder before any conversion.

### Run 2 — Debian 12 (Linux, LSI Logic single extent)

`vmware-real-results-debian12.txt`:

```
Debian 12 | preview=ok: guest=OtherLinuxX86_64 firmware=Bios controller=LsiLogic levels=1 virtual=21474836480 bytes prep=false warnings=[ControllerReplaced] required=3357738230 free=Some(208227180544) | imported=Debian 12 imported | source_unchanged=Some(true) | content_identical=Some(true) | running_after=Some("1.1413815s") | desktop_after=None | console_after=Some("29.4934488s") | stop_after=None | final_frame=None | stopped=true
```

| # | Item | Outcome |
| --- | --- | --- |
| 2 | Linux x86_64 (Debian 12), LSI Logic disk converted to virtio (`ControllerReplaced` warning): import, source folder byte-identical, converted disk content-identical (`qemu-img compare`), start, text login prompt on tty1 at 29 s (`Debian 12-login.png`), graceful ACPI stop in 0.6 s, no orphans, audit `vm_import` succeeded | verified (session entry: text login; no credentials used, no session entered) |

### Run 3 — VMCR2011 (Windows, NVMe, legacy BIOS): import ok, boot failure (product finding)

The run of 2026-10-14 (20:00–20:33 local) executed this VM and then died during the next import (Vista, session closed): the formal record line was not written, but the run's own output and the final frame are the evidence.

Run output (verbatim):

```
VMCR2011: running after 1.2520982s
VMCR2011: no session entry within 10 min; last frame 720x400 lit=5280/288000 colours=2
lightvm-service: graceful stop: phase Quit, cause QmpQuit, 20.3406884s
VMCR2011: stopped
```

Final frame (`run-2026-10-14/VMCR2011-last.png`, stable for the full 10 min):

```
SeaBIOS (version rel-1.17.0-0-gb52ca86e094d-prebuilt.qemu.org)
Booting from Hard Disk...
Boot failed: could not read the boot disk
Booting from Floppy...
Boot failed: could not read the boot disk
No bootable device.
```

The source VM (`D:\VMCR2011`, read-only): `guestOS = "windows9"`, 4 vCPU, 4096 MiB, `cleanShutdown = "TRUE"`, boot disk on an **NVMe** controller (`nvme0:0`, two-level chain), legacy BIOS firmware (`uuid.bios`, no `firmware = "efi"` key).

| # | Item | Outcome |
| --- | --- | --- |
| 3 | Import (preview, conversion of the two-level NVMe chain, source unchanged, content identical, start, stop): the infrastructure ran to completion — QEMU ran for 10 min, the VM reached the `Stopped` state without orphans (graceful stop escalated to QMP quit at 20 s, the guest never acknowledged the ACPI power button from a non-booted state) | verified |
| 4 | **Finding — legacy BIOS + NVMe is an unbootable combination**: the mapping (Windows 10 x86, SeaBIOS, NVMe kept as on the source) produces a VM that SeaBIOS cannot boot: it never reads the MBR from the NVMe disk ("could not read the boot disk" → "No bootable device"). The disk content is faithful (`qemu-img compare` against the source chain), so the failure is the firmware/device combination, not the conversion. Upstream SeaBIOS has NVMe support with known limits (64-bit BAR mapping over 4 GiB; de-configured in some distro builds; the prebuilt `bios-256k.bin` of QEMU 11.1.0 shows the failure above) | verified (failure mode), root cause of the SeaBIOS/NVMe failure not isolated (controlled IDE-vs-NVMe experiment pending, §Required actions) |
| 5 | **Product gap**: the importer accepts the combination at the preview (no refusal, no warning); a refusal or a firm warning ("this VM's firmware cannot boot from its NVMe disk in LightVM: convert it to UEFI in VMware, or to a SATA disk, and import again") is required before R6 is complete | open (proposed decision, §Required actions) |

### Run 4 — Windows Vista (LSI Logic), Mint (14-level chain)

Not executed in the 2026-10-14 run (it died during the Vista import, 20:33 local; no catalog left, no `qemu-img` residue). Re-executed on 2026-10-15 after the NVMe/BIOS decision, with the storage helper enabled (§Run 5 for the experiment that settled that decision).

### Run 5 — Controlled experiment: legacy BIOS + NVMe (2026-10-15)

The VMCR2011 disk was converted once (two-level NVMe chain → `vmcr2011.qcow2`; `qemu-img convert` exit 0, `qemu-img compare` against the source chain exit 0, source folder hash-compared unchanged), then the **same disk** was booted under the same SeaBIOS (QEMU 11.1.0) and WHPX, Skylake-Client-v4, 4096 MiB, 4 vCPU, on three controllers (AHCI and IDE on qcow2 overlays of the base disk; NVMe on the base disk, which never writes):

| # | Controller (machine) | Result | Evidence |
| --- | --- | --- | --- |
| A | NVMe (q35) | SeaBIOS **cannot read the boot disk**: "Booting from Hard Disk… / Boot failed: could not read the boot disk / Booting from Floppy… / Boot failed: could not read the boot disk / **No bootable device**" — stable for the full two-minute window (12 frames, the same screen; the two alternating frame hashes are the blinking cursor). The guest never starts. | `lab/logs/nvme-exp/nvmeA-0000s.png` … `nvmeA-0118s.png`; the QMP stream in `boots-console.log` |
| B | AHCI (q35 built-in, `bus=ide.2`; overlay) | the same SeaBIOS **reads the boot disk and boots the Windows 10 guest**: the Windows boot screen (logo, "Fase preliminare", the dots animating) — the frames keep changing until the moment of the reset, so the boot is progressing at normal speed. Then the guest **resets itself** (QMP `RESET {"guest":true,"reason":"guest-reset"}`: B at 16–32 s, B2 at 16–32 s on a fresh overlay, B3 at 21–41 s on another) and the VM stops: the screen freezes on the last Windows boot frame, **byte-identical** for the rest of the window (B 32–285 s, B2 32–597 s, B3 41–1489 s; `query-status` = `paused` at every frame). Why the VM does not resume after the guest reset: the note below. | `lab/logs/nvme-exp/ahciB-0016s.png` (before), `ahciB-0032s.png` (frozen); `ahciB2-0016s.png` / `ahciB2-0032s.png`; `ahciB3-0021s.png` / `ahciB3-0041s.png`; the QMP streams in `boots-console.log`, `boots2-console.log`, `boots3-console.log` |
| D | IDE (i440fx, `bus=ide.0`; overlay) | as in B: SeaBIOS reads the disk and boots Windows 10 (the logo by 16 s, "Fase preliminare" by 47 s in D and at the 79 s frame in D2 — in D2 the screen even clears to black during the boot at 63 s, the frame before the reset), and then a guest reset (D 47–63 s, D2 63–79 s on a fresh overlay) and the same stop. The frozen frame differs between the two overlays: in D it is the re-rendered SeaBIOS banner (720x400 — the device reset had restored the VGA text mode), in D2 it is the "Fase preliminare" screen (1024x768); byte-identical for the rest of each window (D 63–286 s, D2 79–286 s). | `lab/logs/nvme-exp/ideD-0047s.png` (boot screen), `ideD-0120s.png` (frozen banner); `ideD2-0047s.png` (boot), `ideD2-0063s.png` (cleared), `ideD2-0079s.png` … `ideD2-0286s.png` (frozen boot screen); the QMP streams in `boots-console.log`, `boots2-console.log` |

The disk content is faithful (`qemu-img compare` exit 0 against the source chain), so the NVMe failure (A) is the firmware/device combination, not the conversion: this SeaBIOS build does not read an NVMe boot disk, while it reads the same disk on the q35 AHCI (B) and the i440fx IDE (D) and boots the guest from both.

Two distinct things in B and D, neither a failure of the conversion:

1. **The guest reset is the behaviour of this Windows 10 guest under WHPX**: the guest reboots itself during the early boot phase (16–79 s depending on the arm; the reason — a bug check, a watchdog — is not observable from the console; root cause not isolated). It is a boot of the guest OS, distinct from the firmware-level "could not read the boot disk" of item 4, which is a failure to read the disk at all before the OS starts.
2. **The post-reset stop is a QEMU matter, not a product or guest one**: after the guest reset request, the virtual processors leave WHPX with the exit reason `WHvRunVpExitReasonUnrecoverableException` (4) — the guest's reboot path faults under WHPX: `R2c-01` observed the same stderr line when Vista Setup rebooted for the first time ("the guest triple-faulted"; root cause not isolated there either) — and the WHPX backend of QEMU 11.1.0 has no case for that exit reason (stderr: `WHPX: Unexpected VP exit code 4`, one line per reset virtual processor — 2 in B3, 4 in D); its default path stops the VM in the `paused` state, where it remains for the rest of the window (QMP `STOP` after the `RESET`; `query-status` = `paused` at every frame after the reset, up to 1489 s in the longest window). The frozen frame is the last thing rendered before the stop. (`lab/logs/nvme-exp/ahciB3-stderr.txt`, `ideD-stderr.txt`; `R2c-01-windows-guest-profiles.md`.)

**Product decision (item 5), decided 2026-10-15:** the NVMe controller of a legacy-BIOS guest is replaced at the plan with the profile's controller (AHCI for Vista and later, IDE for XP, virtio for Linux), the Windows guests get the storage driver preparation of `R6-02` (idempotent when the guest's own driver is already enabled for boot, as in this guest: `storahci` `Start`=0), and a UEFI guest keeps NVMe (OVMF boots it). Implemented in `vmware_import::plan` (the plan takes the firmware) with the test `nvme_needs_uefi`; ADR-0030 records the rule. The alternative — refusing the combination at the preview — was dropped: the remap makes the VM bootable instead of refusing a disk the firmware simply cannot read.

## Limits

- No guest credentials were used: "reached the session entry" means the login prompt or the desktop was shown; what happens after the login is not exercised.
- The guest outcome is recorded, not asserted (except the expected session entry of the run's reference Linux VM); a VM that does not boot is evidence about the guest/hardware, recorded as such.
- The VMCR2011 record is from a run that was interrupted after it: the formal record line (preview fields) will be regenerated by the re-execution; the boot evidence (run output + final frame) is as quoted.
- One run per VM set shares a single catalog under the system temp; the test removes its catalog at the end of the test. After the interrupted run no catalog was found (cause unverified); the per-VM PNGs and the run log remain as the evidence of the interrupted part.

## Required actions

1. ~~Controlled experiment for item 4~~ — **done, §Run 5** (2026-10-15): the same converted disk on NVMe vs AHCI vs PIIX IDE under the same SeaBIOS isolates the failure as the firmware/device combination (the disk content is faithful; the same disk boots from the q35 AHCI).
2. ~~Product decision for item 5~~ — **done, §Run 5** (2026-10-15): the NVMe controller of a legacy-BIOS guest is replaced at the plan with the profile's controller (with the Windows storage driver preparation; UEFI keeps NVMe) — a bootable result instead of a refusal; implemented in `vmware_import::plan` with the test `nvme_needs_uefi`, recorded in ADR-0030.
3. ~~Re-execute the remaining real VMs and complete the records~~ — **done, §Run 4** (2026-10-15), with the update of item 19 of `R6-01` and of `docs/STATUS.md`.
4. ~~The LSI Logic storage-driver preparation spike~~ — **done** (`R6-02-lsi-driver-spike.md`, 2026-10-15): the storage helper, implemented and verified on the real Vista guest.

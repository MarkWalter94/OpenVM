# R6-01b — Real VMware VMs imported and started (continuation of R6-01)

- Execution date (UTC): 2026-10-14
- Executed by: development agent on the project host
- Roadmap step: section 12, R6, required action 1 of `R6-01` (real VMware VMs imported and started)
- Related: `R6-01-vmware-import.md` (parser, hostile inputs, mapping, conversion, journal, IPC, UI), ADR-0030, ADR-0027
- Overall outcome: partial — one Linux VM verified end to end (session entry), one refused VM verified, one product finding (a legacy-BIOS VM with an NVMe disk imports but cannot boot: SeaBIOS cannot read the NVMe boot disk); the remaining VMs (Windows Vista, Mint) were not executed (the run was interrupted during the Vista import)

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

Final frame (`VMCR2011-last.png`, stable for the full 10 min):

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

Not executed: the run died during the Vista import (20:33 local, session closed; no catalog left, no `qemu-img` residue). To be re-executed after the NVMe/BIOS decision (§Required actions), which may change the Vista profile's handling.

## Limits

- No guest credentials were used: "reached the session entry" means the login prompt or the desktop was shown; what happens after the login is not exercised.
- The guest outcome is recorded, not asserted (except the expected session entry of the run's reference Linux VM); a VM that does not boot is evidence about the guest/hardware, recorded as such.
- The VMCR2011 record is from a run that was interrupted after it: the formal record line (preview fields) will be regenerated by the re-execution; the boot evidence (run output + final frame) is as quoted.
- One run per VM set shares a single catalog under the system temp; the test removes its catalog at the end of the test. After the interrupted run no catalog was found (cause unverified); the per-VM PNGs and the run log remain as the evidence of the interrupted part.

## Required actions

1. Controlled experiment for item 4: the same converted disk (or a known-bootable MBR disk) on PIIX IDE vs NVMe under the same SeaBIOS, to isolate the SeaBIOS/NVMe failure mode (firmware support vs BAR mapping vs disk content).
2. Product decision for item 5: refuse (or warn firmly) at the preview when the imported firmware cannot boot the imported disk controller (legacy BIOS + NVMe now; extend the check to other such combinations as they appear), with a `vmware-import` plan test.
3. Re-execute the remaining real VMs (Windows Vista LSI Logic — expected `StorageDriverPreparation` outcome; Mint 14-level chain) and complete the records, then update item 19 of `R6-01` and `docs/STATUS.md`.
4. The LSI Logic storage-driver preparation spike — `R6-02`.

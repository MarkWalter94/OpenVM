# R2c-01 — Windows guest hardware profiles: SeaBIOS under WHPX and pending tests

- Execution date (UTC): 2026-10-12
- Executed by: development agent on the project host
- Roadmap step: section 12, R2 (increment R2c, Windows guest x86_64 on Windows 11/WHPX)
- Related ADRs: ADR-0027 (Windows guest hardware profiles), ADR-0011, ADR-0007, ADR-0004, ADR-0010
- Overall outcome: partial (SeaBIOS + WHPX spike and the product command line of every profile verified with the Ubuntu ISO; every Windows guest test not executed)

## Purpose

ADR-0011 excludes legacy BIOS because SeaBIOS with `-cpu host` under WHPX pauses with `WHPX: Unexpected VP exit code 4` (`F0-02` §9.1). ADR-0027 adopts SeaBIOS for the Windows profiles with the nominal CPU model already used by the product. This file records:

1. a spike that checks whether SeaBIOS, passed explicitly with `-bios`, boots under WHPX with the nominal CPU model `Skylake-Client-v4` on `pc-q35-11.1` and on `pc-i440fx-11.1`;
2. the list of tests still to be run for each Windows guest system (section "Pending").

What this test does NOT demonstrate: anything about a Windows guest. The spike boots the Ubuntu reference ISO, uses `-drive` instead of the product's `-blockdev` command line, has no disk, no `usb-tablet`, no `ide-hd`, no input and no ACPI shutdown. It does not update any cell of `docs/COMPATIBILITY.md` beyond the component row for SeaBIOS.

## Environment

| Item | Value |
| --- | --- |
| Host (model, CPU, RAM, storage) | Intel Core i7-12800HX, 31.6 GiB RAM, volume E: |
| OS and build | Windows 11 Pro 10.0.26200 |
| Accelerator and state | WHPX (`-accel whpx`) |
| Toolchain / relevant versions | Windows PowerShell 5.1 (spike script) |
| QEMU binary (version, origin, SHA-256) | QEMU 11.1.0, `lab/qemu/qemu-11.1.0/qemu-system-x86_64.exe`, SHA-256 `47d57a6072e0bb3bd98f87926eb129eb1736dfe818c67b3b81ef7ce4edd0b3cd` (ADR-0010; recomputed on 2026-10-12: matches) |
| Firmware (file, version, SHA-256) | SeaBIOS `share/bios-256k.bin` of the same build, 262 144 bytes, SHA-256 `ae6f6aa973aaccc143f57aa960fb035fd9de4daee4ad0cd713322f8c259e7650` (matches `F0-02` §2.3; recomputed on 2026-10-12). Version shown on screen in F0: `rel-1.17.0-0-gb52ca86e094d-prebuilt.qemu.org` |
| Guest / media (name, version, SHA-256) | `lab/media/ubuntu-26.04.1-desktop-amd64.iso`, SHA-256 `601e30fbf5d97759367c632e2c33630665039b7e2158fd068403da3ccf1bda1f` (recomputed on 2026-10-12: matches) |
| Graphics session (local, RDP, headless) | `-display none`; screenshots through the human monitor `screendump` |
| Other conditions (antivirus, group policies, etc.) | as in `F0-02`; Hyper-V and VBS/HVCI active |

## Procedure

Script `lab/logs/seabios-whpx-spike.ps1` (git-ignored, in `E:\OpenVM\lab\logs\`). It starts QEMU with the arguments below, connects to a loopback human monitor, takes a `screendump` at the requested seconds after the start, then sends `quit` (and kills the process if it is still alive):

```text
qemu-system-x86_64.exe -no-user-config -nodefaults -accel whpx -machine type=<machine>
  -cpu Skylake-Client-v4 -m 3072 -smp 2 -bios <QEMU>\share\bios-256k.bin
  -device VGA -nic none -display none -monitor tcp:127.0.0.1:<port>,server,nowait
  -drive if=none,id=cd0,media=cdrom,readonly=on,file=<ubuntu-26.04.1-desktop-amd64.iso>
  -device ide-cd,drive=cd0,<bus>,bootindex=0
```

| Run | `<machine>` | `<bus>` |
| --- | --- | --- |
| q35 | `pc-q35-11.1` | `bus=ide.0` |
| i440fx | `pc-i440fx-11.1` | `bus=ide.1,unit=0` |

Files kept in `E:\OpenVM\lab\logs\` (outside the repository): `seabios-whpx-spike.ps1`, `seabios-whpx-q35-6.png`, `seabios-whpx-q35-25.png`, `seabios-whpx-q35-100.png`, `seabios-whpx-fx-25.png`, `seabios-whpx-fx-100.png`, `seabios-whpx-q35.stderr.txt`, `seabios-whpx-fx.stderr.txt`. The number in each PNG name is the second of the screenshot.

## Results

| Item | Outcome | Evidence |
| --- | --- | --- |
| SeaBIOS + `Skylake-Client-v4` under WHPX, `pc-q35-11.1`: boot loader | verified | GRUB menu of the ISO ("Try or Install Ubuntu") in `seabios-whpx-q35-6.png` and `seabios-whpx-q35-25.png` |
| Same, `pc-q35-11.1`: kernel | verified | Plymouth splash at 1280×800 at 100 s, `seabios-whpx-q35-100.png` |
| SeaBIOS + `Skylake-Client-v4` under WHPX, `pc-i440fx-11.1`: boot loader | verified | GRUB menu at 25 s, `seabios-whpx-fx-25.png` |
| Same, `pc-i440fx-11.1`: kernel | verified | Plymouth splash at 1280×800 at 100 s, `seabios-whpx-fx-100.png` |
| No `Unexpected VP exit` | verified (both runs) | the stderr files contain no WHPX error |
| stderr content | recorded | both files contain only `host doesn't support requested feature: CPUID[eax=06h].EAX.arat [bit 2]` (twice) and `Ignoring request for interrupt vector 0` |

Reference for the failure that motivated ADR-0011: `lab/logs/fw-seabios-iso.stderr.txt` (F0, 2026-10-09, SeaBIOS with `-cpu host`) contains `Ignoring request for interrupt vector 0` and `WHPX: Unexpected VP exit code 4`.

No measurement samples are taken in this spike (single run per machine type).

## Limits

- Linux ISO only. No Windows installation medium was booted; nothing here shows that any Windows version installs or runs.
- Not tested: `usb-tablet` (with `qemu-xhci` or `piix3-usb-uhci`), `ide-hd` on `ide.2` (AHCI) or on `ide.0` (i440fx), installation to disk, reboot from disk, Windows ACPI shutdown, the PS/2 keyboard through the console, the per-profile accelerator probe of the product, baseline and restore without NVRAM.
- One run per machine type; the screenshots show the boot progress at fixed seconds, not the end of the boot.
- The command line is the spike's (`-drive`, human monitor over TCP), not the product's (`-blockdev`, private QMP and console channels).
- The `arat` CPUID warning means the host does not expose that feature of the nominal model; its effect on Windows guests is not known.

## Pending

Tests to run on this host for each guest system of ADR-0027, each with its own real installation ISO (identified by name and SHA-256) and the product's command line. None is executed.

Common to every system and firmware:

1. Detection at import: the wizard shows the expected system, architecture and build; correcting it by hand works.
2. Accelerator probe of the product with the profile's machine type and firmware (ADR-0007): passes, or the start is refused with an explicit error.
3. Devices observed with QMP match the profile; no NIC (`info network`, `query-pci`; ADR-0004).
4. Installation to the `ide-hd` disk from the installation CD, with the data CD attached and read-only.
5. Reboot from the disk after removing the installation medium; the installed system reaches the desktop.
6. Pointer (`usb-tablet`) follows the console's absolute coordinates, also after resizing; keyboard (PS/2) with the IT and US layouts.
7. Orderly stop (ACPI power button) completes within the supervisor timeout; forced stop leaves no orphan processes.
8. Baseline with the VM off, change in the guest, restore: the disk returns to the baseline state; for UEFI the vars are restored and the firmware hash is recorded; for Legacy BIOS the firmware hash is absent.

Per system:

| System | Profile(s) to test | Specific checks |
| --- | --- | --- |
| Windows XP (x86; x64 if a medium is available) | `pc-i440fx-11.1`, SeaBIOS, `ide-hd` on `ide.0` unit 0, CDs on `ide.1`, `piix3-usb-uhci` + `usb-tablet` | text-mode setup sees the IDE disk without an F6 driver; USB tablet driver loaded by the installed system |
| Windows Vista (x86, x64) | `pc-q35-11.1`, SeaBIOS, AHCI `ide.2`, `piix3-usb-uhci` + `usb-tablet` | AHCI disk seen by setup with inbox drivers; VGA resolution available after installation |
| Windows 7 (x86, x64) | as Vista | as Vista |
| Windows 8.1 (x86, x64) | `pc-q35-11.1`, UEFI (x64) and SeaBIOS (x86 and x64), AHCI `ide.2`, `qemu-xhci` + `usb-tablet` | UEFI boot of the x64 medium with Secure Boot off; vars persistence across reboots; BIOS default for x86 media |
| Windows 10 (x86, x64; build below 22000) | as Windows 8.1 | as Windows 8.1 |
| Windows 8 (6.2) and Windows 11 (build 22000 and later) | none | the media produce no suggestion and the systems are not offered (negative test) |

Detection of real Windows installation media: recorded below for Windows Vista and Windows 10 (section "Real Windows installation media"); pending for XP, 7 and 8.1.

## Product command line with the Ubuntu ISO (2026-10-12)

After the spike the profiles were implemented in `qemu-backend`, `lightvm-service`, `vm-core`, `vm-storage` and `guest-detect`. Two ignored real tests run the product's own command line (`-blockdev`, private QMP and console channels, per-profile accelerator probe) with the Ubuntu reference ISO as installation medium. They show that each profile's machine, firmware, controllers and display start under WHPX; they still say nothing about a Windows guest.

| Test | Command | Outcome |
| --- | --- | --- |
| `qemu-backend` `real_windows_hardware_profiles` | `LIGHTVM_QEMU_DIR=E:/OpenVM/lab/qemu/qemu-11.1.0 cargo test -p qemu-backend --test real_qemu real_windows_hardware_profiles -- --ignored --test-threads=1` | passed. Profiles Windows 10 BIOS (q35, xHCI), Windows 7 BIOS (q35, UHCI), Windows XP BIOS (`pc-i440fx-11.1`), Windows 10 UEFI (q35, AHCI), each with an empty 1 GiB qcow2 disk: accelerator probe "Available" per (machine family, firmware), state Running, no NIC (`verify_no_nic`), non-black display, no orphan process. Log `lab/logs/f1-03/s1791813424183-profiles.log` |
| `lightvm-service` `windows_real` | `LIGHTVM_E2E_QEMU_DIR=E:\OpenVM\lab\qemu\qemu-11.1.0 cargo test -p lightvm-service --test windows_real -- --ignored` | passed. Through the real service: import of the Ubuntu ISO detected as `UbuntuLtsX86_64`; VM declared Windows 7 (BIOS) reached Running with a 720×400 frame (BIOS GRUB text menu); VM declared Windows 10 (UEFI) reached Running with a 1280×800 frame (UEFI GRUB menu); screenshots in `lab/logs/win-profiles-service/`. The BIOS VM had no vars file, the UEFI VM had one. Both stops escalated from the ACPI power button to QMP `quit` after the 20 s timeout because the live ISO ignores ACPI at its GRUB menu; no QEMU process was left |
| Linux path unchanged | `core_path` and `f2` real tests of `lightvm-service`; `real_qemu` probe, start/stop cycles, path rejection and NIC detector of `qemu-backend` | passed |

Parser robustness on synthetic input (`cargo test -p guest-detect`): truncation of every image at every half sector, malformed WIM headers, compressed XML resources, and three property tests (mutated UDF images, mutated ISO 9660 images, random bytes; 512 cases each) return an error or "not recognized" without panic.

Storage: migration `0007_media_detected_guest.sql` adds the detected guest to the media registry (`detected_guest` JSON, `guest_examined` flag). Media imported before the migration are examined once, the first time the media list is read, and the result is stored.

## Real Windows installation media (2026-10-13)

Host: Windows 11 x86_64, WHPX, QEMU 11.1.0 (`lab/qemu/qemu-11.1.0`), CPU `Skylake-Client-v4`. Media provided by the user, not redistributed: Windows Vista Ultimate SP2 x86 and x64 it-IT (2020-01 images, build 6003), Windows 10 22H2 it-IT x64 and x86 (`lab/media`). The criterion is "Windows Setup shown", not an installation: none of the checks 2–8 above has been run yet.

**Detection** (`cargo test -p guest-detect --test detect real_isos -- --ignored --nocapture`, `LIGHTVM_DETECT_ISO_DIR` on each folder): Vista x64 → `WindowsVista`, `X86_64`, build 6003, *Windows Vista Ultimate*; Vista x86 → `WindowsVista`, `X86`, build 6003; Windows 10 x64 → `Windows10`, `X86_64`, build 19041, *Windows 10 Home*; Windows 10 x86 → `Windows10`, `X86`, build 19041; the Ubuntu reference ISO stays `UbuntuLtsX86_64`. Each read took a few milliseconds.

**Through the service** (`lightvm-service` `windows_real` `e2e_real_windows_iso_reaches_setup`, ignored; `LIGHTVM_E2E_QEMU_DIR=E:\OpenVM\lab\qemu\qemu-11.1.0`, `LIGHTVM_E2E_WINDOWS_ISOS=<the four ISOs>`, `LIGHTVM_WIN_EVIDENCE_DIR=<folder of the frames>`). Each ISO is imported, the VM is created with the detected system, its template and the recommended firmware for the detected architecture, started, and watched on the console until a lit frame with at least 64 distinct colours (the Setup window; a stop error screen or a firmware shell has 2–4 colours) or 10 minutes; then stopped. Outcome: passed, 813 s.

| Medium | VM | Outcome |
| --- | --- | --- |
| Vista x64 | Vista, Legacy BIOS | refused by the service at creation (`unsupported`, `field=install_media`, `arch=x86_64`): rule of ADR-0027 for Windows hosts |
| Vista x86 | Vista, Legacy BIOS, q35, AHCI, UHCI tablet | Setup language page (it-IT), 800×600, after "Windows is loading files"; stop by QMP `quit` after the 20 s ACPI timeout (Setup ignores the power button) |
| Windows 10 x64 | Windows 10, UEFI, q35, AHCI, xHCI tablet | the backend pressed Enter at the CD prompt (service log: "25 sent, 0 failed, end WindowElapsed"); Setup language page, 1280×800, shown after the 25 s window had ended, on its first page |
| Windows 10 x86 | Windows 10, Legacy BIOS | Setup language page, 1024×768 |

No QEMU process was left after any VM or at the end.

**Vista x64 under WHPX.** Before the rule, the same VM stopped with `STOP 0x0000005C (0x10B, 3, 0, 0)` (HAL_INITIALIZATION_FAILED) right after "Windows is loading files". Reproduced with QEMU alone (`-bios bios-256k.bin`, the Vista x64 ISO, an empty 40 GiB qcow2, a QMP screendump after 90 s) in twelve variants, all with the same stop: `pc-q35-11.1`; `pc-i440fx-11.1` with IDE; `-cpu Skylake-Client-v4,-x2apic`; `-hypervisor`; both; `Nehalem`; `qemu64`; one vCPU; `hpet=off`. With `-accel whpx,kernel-irqchip=off` (with or without `-hypervisor`, with one or two vCPUs) the HAL initializes and Setup stops later with `STOP 0x0000000A` (IRQL_NOT_LESS_OR_EQUAL). Every in-kernel-irqchip run logs `warning: Ignoring request for interrupt vector 0`. Reading (not verified in the QEMU sources): the 64-bit HAL calibrating its timer gets an interrupt in virtual-wire mode that the WHPX interrupt path drops. Vista x86 boots on the same profile. Decision of the project owner: Vista 64-bit not supported on Windows hosts (ADR-0027).

**Windows 10 x64 before the key.** Without the backend pressing a key, the same VM showed "Guest has not initialized the display (yet)", then the UEFI Interactive Shell with the CD and the disk mapped: the Windows UEFI loader's "Press any key to boot from CD or DVD..." prompt timed out. The criterion of a blank disk and the 25 s window are in ADR-0027. Residual risk: on a host where Setup appears within 25 s of the vCPUs starting, an Enter would press the default button of Setup's first page (Next); on this host Setup appeared after the window.

**Test bug found on the way.** The first version of the test accepted any frame with 1 % lit pixels and took the Vista x64 stop screen for Setup; a second version required that no colour covered 90 % of the frame and rejected the flat purple background of the Windows 10 Setup. The distinct-colour criterion above separates the recorded frames (stop screens 2 colours, UEFI Shell 4, Windows 10 Setup 852).

## Vista x86 installation: virtual CPU error at the first Setup reboot (2026-10-14)

Host as above (Windows 11 x86_64, WHPX, QEMU 11.1.0). The user installed Vista x86 from the desktop app (VM Vista, Legacy BIOS, q35, AHCI, UHCI tablet, the profile of the table above).

**Observed.** About 22 minutes into Setup, at its first reboot, the QEMU log of the run recorded `WHPX: Unexpected VP exit code 4` twice. Exit code 4 is `WHvRunVpExitReasonUnrecoverableException`: the guest triple-faulted. Real hardware or KVM resets the machine at that point. Under WHPX, QEMU 11.1 stops the virtual CPUs and moves the guest to run state `internal-error` (QMP `STOP`), while the process stays alive. LightVM did not notice. The VM stayed *Running* in the app, the console showed the last frame, and every keyboard and mouse batch was refused as `input refused: infrastructure_error`. In the service log that refusal reads `QEMU rejected input-send-event: GenericError: VM not running`. The installation could only go on after a forced stop and a new start. Not investigated: why the reboot triple-faults under WHPX (Vista's reboot path, the SeaBIOS reset vector or WHPX itself).

**New behaviour** (`qemu_backend::vcpu_fault`, run supervision in `docs/ARCHITECTURE.md`):

- The run follows its QMP events. On `STOP`, it reads `query-status`. For `internal-error`, it does what hardware does on a triple fault: `system_reset`, then `cont`. QEMU moves the guest to `prelaunch` after a reset from `internal-error` and refuses `cont` until the reset has happened, so the state is polled in between.
- At most 3 automatic resets within 120 s. A 4th fault in that window leaves the guest stopped. The VM summary then reports `vcpu_halt` (`internal_error`, number of automatic resets). The Dashboard shows the notice "Stopped on a virtual CPU error…" with Reset and Force stop. The console shows the same text in place of the refusal code, and the service refuses input with `state_conflict` (`reason=vcpu_halted`).
- Every decision goes to the service log and to the audit, as `vm_auto_reset` with actor `system`: `succeeded`, `failed`, or `denied` with `resource_limit` when the limit is reached.
- The Reset action works on such a guest: reset, then `cont`, then the halt is cleared.
- `guest-panicked` is never reset automatically. It is reported as a halt.

**Verified** with the synthetic QMP server only (`qemu-backend` `tests/qmp_mock.rs`: automatic reset and `cont` twice; limit reached, left in `internal-error`, then the user's reset resumes it), with the policy unit tests (`vcpu_fault`), the supervisor test (summary, state event, audit, reset), and the Dashboard and console UI tests. Not yet verified: the same Vista x86 installation with the new build (whether one automatic reset lets Setup continue).

**Correction (2026-10-16).** The QEMU 11.1.0 source handles an unexpected WHPX exit with `vm_stop(RUN_STATE_PAUSED)` (`target/i386/whpx/whpx-all.c`), so the run state is `paused`, not `internal-error`; the watcher above therefore never reset a WHPX guest. A Windows 11 installation stopped this way and stayed black for 40 minutes with no fault event (`R6-05` §14). LightVM never sends QMP `stop` and disk errors use `io-error`, so the watcher now treats `paused` like `internal-error` (reset, then `cont`; a paused guest stays `paused` after the reset). Verified with the synthetic QMP server (`whpx_pause_after_an_unrecoverable_exit_is_reset_and_resumed`) and seen on real Windows 11 faults (automatic resets logged, `R6-05` §15). That investigation also found that after a reset inside QEMU 11.1.0/WHPX the application processors ran the firmware's reset vector together with the BSP and the firmware triple-faulted (exit code 4); whether this is the cause of Vista Setup's first-reboot fault here is not verified (the Windows 11 profile now reboots cold instead).

## Required actions

- Obtain installation media for the systems above from the user (LightVM does not redistribute Windows images; licences are the user's). Vista and Windows 10 media are available; XP, 7 and 8.1 are missing.
- Install Vista x86 and Windows 10 (UEFI and BIOS) to disk and run checks 2–8.
- Run the pending tests and record them as dated sections appended to this file or in new `R2c-nn` files.

## Proposed decisions

- None beyond ADR-0027. If a profile fails and no alternative of ADR-0027 passes, propose removing that system from the wizard.

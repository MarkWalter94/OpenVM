# ADR-0027 — Windows guest hardware profiles

- Status: Accepted (direction set; evidence pending)
- Date: 2026-10-12
- Owner: to be assigned

## Context

ADR-0011 fixes a single reference guest (Ubuntu 26.04.1 LTS desktop amd64) and a single firmware (UEFI, OVMF from the QEMU build of ADR-0010). It excludes legacy BIOS for two reasons: SeaBIOS with `-cpu host` under WHPX pauses with `WHPX: Unexpected VP exit code 4` (`docs/verification/F0-02-qemu-whpx-qmp.md` §9.1), and the consistency model of the baseline was built around the UEFI NVRAM.

The roadmap lists Windows guests after the MVP (section 4, "Dopo il MVP") and makes them a binding increment of v1.0: **R2c**, "Windows guest x86_64 su Windows 11/WHPX e Ubuntu/KVM, con driver, stato persistente e recupero verificati" (section 12). The same section states that a Windows guest introduces firmware, drivers, licence and, depending on the version, TPM/Secure Boot; that the state of the added devices must be part of the restore; and that **no requirement bypass is assumed as a product feature**. Section 5 records that ordinary support for Windows 10 ended on 14 October 2025 and that the project does not redistribute Windows images.

Facts that shape the hardware choice:

- Windows has no inbox virtio drivers. The Linux profile of ADR-0011 (`virtio-blk-pci`, `virtio-vga`, `virtio-tablet-pci`, `virtio-keyboard-pci`) cannot be used to install Windows without an external driver disc.
- The console sends absolute pointer events (0..32767, ADR-0003). Windows has an inbox driver for a USB HID tablet, not for `virtio-tablet-pci`.
- Windows XP has no inbox AHCI driver and predates the ICH9 chipset of `q35`.
- Windows 11 requires TPM 2.0 and Secure Boot; Windows 8 (6.2) is out of support and has no distinct value over 8.1 for this product.
- The product already avoids `-cpu host`: the default CPU model is the nominal `Skylake-Client-v4` (deviation D-1, `F1-03`; `docs/STATUS.md`). The SeaBIOS failure of §9.1 was observed only with `-cpu host`.

Decision of the project owner (2026-10-12): support Windows XP, Vista, 7, 8.1 and 10 as guests through the normal wizard, with automatic detection of the installation ISO; offline only; a new ADR. Windows 11 and Windows 8 are not supported. Networking for Windows guests is separate work, tied to R1 and to Custom Network.

## Decision

**Scope.** This ADR adds hardware profiles for Windows guests. **ADR-0011 stays in force for Linux guests** (reference guest, UEFI only, profile and CPU) and its text is not changed. This ADR does not change the MVP scope or any exit criterion of the roadmap, and it does not promote any cell of `docs/COMPATIBILITY.md`: the Windows cells stay "not verified" until the evidence listed below exists.

**Supported guest systems:** Windows XP, Windows Vista, Windows 7, Windows 8.1, Windows 10, each in the 32-bit (x86) or 64-bit (x86_64) edition of the user's installation medium. **Not supported:** Windows 8 (6.2) and Windows 11 (build 22000 and later).

**Hardware profiles (normative).** The profile is chosen from the guest system and the firmware; `X.Y` is the versioned machine type of the approved QEMU build (`11.1` for QEMU 11.1.0).

| Guest system, firmware | Machine | Firmware | Main disk | Install CD / data CD | Display | Pointer |
| --- | --- | --- | --- | --- | --- | --- |
| Ubuntu LTS / Other Linux, UEFI (ADR-0011, unchanged) | `pc-q35-X.Y` | OVMF pflash code + per-VM vars | `virtio-blk-pci` | `ide-cd` bus `ide.0` / `ide.1` | `virtio-vga` | `virtio-tablet-pci` + `virtio-keyboard-pci` |
| Windows 8.1 / 10, UEFI | `pc-q35-X.Y` | OVMF pflash code + per-VM vars | `ide-hd` bus `ide.2` (ICH9 AHCI) | `ide.0` / `ide.1` | `VGA` | `qemu-xhci` + `usb-tablet` |
| Windows 8.1 / 10, Legacy BIOS | `pc-q35-X.Y` | SeaBIOS | `ide-hd` bus `ide.2` | `ide.0` / `ide.1` | `VGA` | `qemu-xhci` + `usb-tablet` |
| Windows Vista / 7, Legacy BIOS | `pc-q35-X.Y` | SeaBIOS | `ide-hd` bus `ide.2` | `ide.0` / `ide.1` | `VGA` | `piix3-usb-uhci` + `usb-tablet` |
| Windows XP, Legacy BIOS | `pc-i440fx-X.Y` | SeaBIOS | `ide-hd` bus `ide.0`, unit 0 | `ide-cd` bus `ide.1` unit 0 / unit 1 | `VGA` | `piix3-usb-uhci` + `usb-tablet` |

- **Keyboard:** for the Windows profiles, the board's built-in i8042 PS/2 keyboard, present with `-nodefaults`; QMP `input-send-event` key events reach it. No `virtio-keyboard-pci`.
- **Pointer:** `usb-tablet`, because the console sends absolute coordinates and Windows has no inbox virtio input driver.
- **Allowed firmware per system:** XP, Vista, 7: Legacy BIOS only. 8.1, 10: UEFI or Legacy BIOS, chosen by the user; the default is Legacy BIOS for 32-bit media (the OVMF build is x86_64 only) and UEFI otherwise. Linux: UEFI only. A disallowed combination is rejected by the backend's type constructor and by `vm-core` validation, not only by the UI.
- **i440fx only for XP.** `pc-i440fx-X.Y` is used only by the XP profile; every other profile stays on `q35`.
- **CPU:** the same nominal model as the Linux profile (`Skylake-Client-v4` by default); `host` and `max` stay rejected.
- **Secure Boot** stays off in the UEFI profiles (non-secure OVMF code, as in ADR-0011). No TPM device.

**SeaBIOS.** The legacy firmware is `share/bios-256k.bin` of the approved QEMU 11.1.0 build (ADR-0010), passed explicitly with `-bios <path>`, with the pinned SHA-256 `ae6f6aa973aaccc143f57aa960fb035fd9de4daee4ad0cd713322f8c259e7650` (recorded in `docs/verification/F0-02-qemu-whpx-qmp.md` §2.3). It is verified against that hash like the other approved files of the distribution; a different file prevents the start of a Legacy BIOS VM. The exclusion of ADR-0011 rests on SeaBIOS with `-cpu host`; with the nominal CPU model the R2c-01 spike reached the boot loader and the kernel on both `q35` and `i440fx` without the exit code 4. That spike used a Linux ISO: it is not evidence for any Windows guest.

**Accelerator probe per profile (ADR-0007).** The probe checks WHPX with the machine type and firmware actually used by the VM. A host where the `q35` + OVMF probe passes is not assumed to pass with `q35` + SeaBIOS or `i440fx` + SeaBIOS: each profile is probed on its own, and a failure refuses the start with an explicit error. No TCG fallback.

**Offline unchanged (ADR-0004 not affected).** No profile has a NIC: `-nic none` and the "no unexpected NIC" check stay as they are, and the wizard keeps "Offline" as the only network. USB controllers carry only the virtual tablet; there is no USB passthrough. Files still enter the guest only through the read-only data medium (ADR-0008).

**Baselines.** A baseline of a UEFI VM includes the copy of the per-VM vars as today (ADR-0005, ADR-0011), and its firmware hash is recorded. A Legacy BIOS VM has no writable firmware state: SeaBIOS keeps no NVRAM file, so its baselines contain the disk only and the firmware hash field is absent. The restore model of ADR-0005 is otherwise unchanged; this is how the roadmap's "state of the added devices" requirement is met for these profiles.

**Detection of the installation medium.** When an installation ISO is imported, LightVM reads its contents (ISO 9660/Joliet and UDF) to suggest the guest system, architecture, build and edition: XP from the `I386`/`AMD64` layout and its marker files; Vista and later from the XML metadata of `sources/install.wim` or `sources/install.esd`; Ubuntu and other Linux from `.disk/info`. The result is a **hint** shown in the wizard's System step and **confirmed or corrected by the user**; it never selects a hardware profile without the user seeing it. Media that report Windows 8 (6.2) or Windows 11 (build 22000 or later), and unknown media, produce no suggestion. The ISO is **untrusted input**: the parser caps the directory entries visited, the path depth and the bytes read (for example 1 MiB of XML), never allocates from a size read from the image without a cap, and returns "not recognized" instead of panicking on malformed data. The parser runs on the managed read-only copy of the media registry (ADR-0008).

**Excluded: Windows 11 and Windows 8.**

- Windows 11 requires TPM 2.0 and Secure Boot. Offering it without them would need a requirement bypass, which the roadmap excludes as a product feature (section 12, R2). Supporting it properly needs an emulated TPM with its own persistent state in the baseline and a tested Secure Boot firmware: separate work with its own ADR.
- Windows 8 (6.2) is out of support and is superseded by 8.1 on the same hardware profile; it is not offered and is not detected as a supported system.

**Licensing and activation** are the user's responsibility. LightVM does not provide, store or bypass product keys or activation, and does not redistribute Windows images (section 5). The user needs their own installation medium and a valid licence.

**End-of-support guests.** Windows XP (support ended 8 April 2014), Vista (11 April 2017), 7 (14 January 2020), 8.1 (10 January 2023) and 10 (14 October 2025, outside ESU/LTSC editions) receive no ordinary security updates. The offline profile mitigates the exposure (no network path in or out of the guest) but does not make these systems safe: software brought in through the data medium runs on an unpatched system. The UI and the user guide state this.

## Alternatives considered

- **virtio-win driver ISO** (keep the virtio devices and load drivers during setup): rejected for this increment. It adds a third-party driver medium whose provenance, signature, licence and per-version coverage (no current drivers for XP and Vista) must be qualified, and it makes the installation depend on manual driver loading. The emulated AHCI/IDE, VGA and USB tablet devices work with inbox drivers. It can be evaluated later for performance, with its own ADR.
- **UEFI only for Windows** (keep ADR-0011's single firmware): rejected. XP, Vista and 32-bit Windows do not boot with the x86_64 OVMF build, and Vista/7 x64 under UEFI depend on a legacy video path that OVMF does not provide. UEFI stays the default for 64-bit 8.1 and 10.
- **i440fx for all legacy guests:** rejected. Vista and later have inbox AHCI drivers and work on `q35`, which keeps the same machine family as the Linux profile and the same CD layout (`ide.0`/`ide.1`). `i440fx` is kept only where it is needed (XP).
- **TPM emulation (swtpm) and Secure Boot for Windows 11:** rejected for this increment. It needs a separate process with persistent state that must enter the baseline, a Secure Boot firmware with SMM not yet tested (ADR-0011), and its own security review. Bypassing the checks of the Windows 11 setup is excluded by the roadmap.

## Consequences

- New `GuestOs` values, `Firmware::Bios` next to `Firmware::Uefi`, a detected-guest record on media and an optional firmware hash on baselines (`lightvm-proto`); per-profile command lines in `qemu-backend`; a bounded ISO parser in the `guest-detect` crate; the wizard's System step prefilled from the detection.
- The baseline and restore code distinguishes VMs with and without NVRAM. The firmware and the hardware profile of a VM are fixed at creation: an update that changes the firmware, or changes the system between Windows and Linux or between two Windows releases, is refused.
- The media registry stores the detected guest (storage migration 0007); media imported earlier are examined once when the media list is first read. Linux guests keep the disk floor of 4 GiB accepted by the service, while the wizard proposes at least 8 GiB.
- Tests: profile command lines and rejected combinations (synthetic); parser malformed-input cases; real install, reboot and baseline/restore per system on this host (below). Until then the Windows matrix cells stay "not verified".
- R2c also names Ubuntu/KVM hosts: the Linux host cells are not addressed by this ADR and stay "not verified" until R2a/R2c tests on a physical Linux host.
- If a profile fails on real hardware and no alternative listed here passes, that system is removed from the wizard and this ADR is updated; the roadmap requirements are not relaxed.

## Required evidence

- SeaBIOS + WHPX with the nominal CPU on `pc-q35-11.1` and `pc-i440fx-11.1`: `docs/verification/R2c-01-windows-guest-profiles.md` (spike with a Linux ISO recorded; no Windows guest yet).
- For each of XP, Vista, 7, 8.1 (UEFI and BIOS) and 10 (UEFI and BIOS), on this host: detection of a real installation ISO; per-profile accelerator probe; installation to the `ide-hd` disk; reboot from disk; USB tablet pointer and PS/2 keyboard through the console; ACPI shutdown; no NIC observed (`info network`, `query-pci`); baseline and restore with the disk (and vars for UEFI) returned to the baseline state. Recorded in `R2c-01` (pending section) or in later `R2c-nn` files.
- Malformed-media tests of the parser (truncated, looping or oversized structures) without panic and within the caps.

## References

Roadmap, sections 4, 5, 7.2, 12 (R1, R2, R2c). ADR-0003, ADR-0004, ADR-0005, ADR-0007, ADR-0008, ADR-0010, ADR-0011. `docs/verification/F0-02-qemu-whpx-qmp.md` §2.3, §9.1; `docs/verification/F1-03-real-qemu-backend.md` (F1-17, nominal CPU model).

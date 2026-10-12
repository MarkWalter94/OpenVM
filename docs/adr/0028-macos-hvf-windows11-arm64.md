# ADR-0028 — macOS Apple Silicon host and Windows 11 ARM64 guest

- Status: Proposed (direction requested by the project owner on 2026-10-12; evidence partial)
- Date: 2026-10-12
- Owner: to be assigned

## Context

The roadmap assigns macOS Apple Silicon to increment **R2b** with Linux ARM64 guests under HVF (sections 5 and 12). ADR-0027 adds x86 Windows guests (XP…10) and excludes Windows 11, because Windows 11 requires TPM 2.0 and Secure Boot and the roadmap excludes requirement bypasses as a product feature; it states that proper support needs "an emulated TPM with its own persistent state in the baseline and a tested Secure Boot firmware: separate work with its own ADR". This is that ADR, for one host only.

Facts:

- Hypervisor.framework (HVF) accelerates only AArch64 guests. On Apple Silicon the x86 profiles of ADR-0027 would run only under TCG, which ADR-0007 excludes. The only Windows that runs accelerated is **Windows 11 ARM64** (Windows 10 on ARM was never sold as installation media).
- Spike `docs/verification/R2b-01-macos-hvf-windows11-arm64.md`: under HVF the official Windows 11 ARM64 setup **passes the requirements check without any bypass** with an emulated TPM 2.0 (swtpm) and a project-built edk2 firmware with Secure Boot on. With the edk2 prebuilt shipped by QEMU (no Secure Boot) the only failing requirement is Secure Boot.
- Apple's Virtualization.framework does not run Windows guests. The other products that run Windows 11 ARM64 on Apple Silicon do it with their own virtual firmware and vTPM (Parallels Desktop, VMware Fusion) or, like this project, with QEMU, an edk2 Secure Boot build and swtpm (UTM).
- macOS has no cgroups and no `PR_SET_PDEATHSIG`: the containment mechanism of ADR-0015 does not apply.

Decision of the project owner (2026-10-12): Windows ARM64 must run on the Mac; build the Secure Boot firmware in the project (option "B") rather than reuse a third-party binary.

## Decision (proposed)

**Scope.** Host: macOS on Apple Silicon, accelerator HVF. Guest: Windows 11 ARM64 from the user's installation medium, **offline only**. This ADR amends ADR-0027 only for this host and architecture: Windows 11 x86_64 and Windows 8 stay excluded; the x86 profiles of ADR-0027 are not offered on this host. Linux ARM64 guests (the roadmap's R2b target) get their profile in a later revision of this ADR or in a separate one. No cell of `docs/COMPATIBILITY.md` is promoted until the evidence below exists.

**Hardware profile (normative once accepted).** `X.Y` is the versioned machine of the QEMU build used on macOS.

| Item | Value |
| --- | --- |
| Machine | `virt-X.Y`, `gic-version=3` |
| CPU | `host` (the only CPU model HVF accelerates; the `host`/`max` exclusion of ADR-0011 concerns OVMF under WHPX) |
| Firmware | project build of §Firmware: code in `pflash0` read-only, per-VM vars in `pflash1` |
| TPM | `tpm-tis-device,ppi=off` with an `emulator` backend on a per-VM swtpm (TPM 2.0); `ppi=on` fails under HVF (`HV_BAD_ARGUMENT`, R2b-01) |
| Main disk | `nvme` (inbox Windows driver), qcow2 overlay through `-blockdev` |
| Install CD / data CD | `scsi-cd` on `usb-bot` (one per medium), read-only; a plain `usb-storage` is seen as a RAW disk and breaks setup (R2b-01) |
| Display | `ramfb` (Windows ARM64 has no inbox virtio-gpu driver) |
| Input | `qemu-xhci` + `usb-kbd` + `usb-tablet` (absolute pointer, ADR-0003) |
| Network | none: `-nic none` and the "no unexpected NIC" check unchanged (ADR-0004) |

**Firmware.** Built by `packaging/firmware/edk2-aarch64/build.sh`: edk2-stable202408 (`b158dad150bf02879668f72ce306445250838201`, the version QEMU 11.1 ships prebuilt), `ArmVirtPkg/ArmVirtQemu.dsc`, `RELEASE`, `CLANGDWARF`, `SECURE_BOOT_ENABLE=TRUE`, `TPM2_ENABLE=TRUE`, `NETWORK_ENABLE=FALSE`, plus one local patch (`0001-BaseRngLib-AArch64-no-writable-global-in-XIP.patch`: the RNDR check stops writing a global from code that runs in place from flash, which faults under HVF). edk2-stable202608 is not used: after the same patch its boot manager countdown never advances under HVF (R2b-01).

- Variable template for Windows guests: Secure Boot on, a platform key generated at build time (private key discarded), Microsoft KEK CA 2011 and 2023, db limited to the Windows signers (Microsoft Windows Production PCA 2011, Windows UEFI CA 2023); no third-party UEFI CA. Enrolment with virt-firmware 26.9.
- The build embeds its time and is not bit-reproducible: each released code and vars file is pinned by SHA-256 in this ADR when the firmware is released, and verified like the other approved files of the distribution (as ADR-0011 does for OVMF). A different file prevents the start.

**Baseline and restore.** A baseline of a Windows ARM64 VM contains the disk, the per-VM vars **and the swtpm state directory**; a restore returns all three. The restore model of ADR-0005 is otherwise unchanged. Consequence to verify: secrets sealed to the TPM (BitLocker, Windows Hello) follow the baseline they were created in.

**Process containment on macOS.** The supervisor starts each QEMU and its swtpm through a watchdog process: the watchdog creates a new process group for them and waits with kqueue (`EVFILT_PROC/NOTE_EXIT` on the supervisor, read EOF on a lifeline pipe held only by the supervisor); on either it sends `SIGKILL` to the group and exits. Evidence R2b-01: 60 cycles with `SIGKILL` to the supervisor, 0 orphans, p95 9.89 ms with QEMU. macOS offers no per-tree CPU/memory limits comparable to cgroups: per ADR-0017 and section 7.2, agent mode stays disabled on this host until such controls exist; manual offline mode is the target.

**Licensing and activation** stay the user's responsibility (as ADR-0027). Microsoft names Parallels Desktop and VMware Fusion as the authorised ways to run Windows 11 ARM on Apple Silicon; LightVM claims no such authorisation, provides no keys and does not redistribute Windows images. Setup uses the official "I don't have a product key" path; no requirement check is bypassed.

## Alternatives considered

- **QEMU's prebuilt edk2 (no Secure Boot) and a setup bypass** (registry `LabConfig` or modified media): rejected, the roadmap excludes requirement bypasses.
- **A third-party Secure Boot build** (Debian `qemu-efi-aarch64`, UTM's firmware): Debian's 2025.02 build hangs at "Start boot option" under HVF (R2b-01); UTM's build was not tested. Rejected by the owner in favour of a project build with known sources.
- **edk2-stable202608**: rejected for now (timer stall under HVF, cause not found).
- **virtio devices with the virtio-win ARM64 drivers**: rejected for the same reasons as in ADR-0027 (third-party driver medium, manual loading); NVMe, USB and ramfb work with inbox drivers.
- **Virtualization.framework**: does not support Windows guests.

## Consequences

- New guest system `Windows11Arm64` and architecture `Aarch64` in `lightvm-proto`; the `GuestArch`/firmware validation of ADR-0027 extended; `guest-detect` stops returning "unsupported" for ARM64 Windows 11 media (it already reads build 26200 / arch 12 from `install.wim`).
- `qemu-backend`: the `virt` profile above, `tpm-tis-device` and the swtpm process (path, private socket, state directory), probe of HVF with this profile (ADR-0007).
- macOS host layer still missing in the product and needed before any VM can start on this host: private directory permissions, host resources, final path of a handle, process identity, the watchdog of §Process containment.
- `vm-storage`: the swtpm state directory joins disk and vars in baselines.
- The firmware becomes a build artefact of the project with its own provenance record and release checks.

## Required evidence

1. Installation to the NVMe disk, reboot from disk, **offline OOBE** for the offered editions (24H2/25H2 setup expects a network; an offline path without bypass must exist, or the edition is not offered and the limit is declared), ACPI shutdown. The licence terms are accepted by the user.
2. Pointer and keyboard through the product console; no NIC observed (`info network`, `query-pci`).
3. Baseline and restore with disk, vars and TPM state returned to the baseline state; TPM-sealed data after restore.
4. The product command line through `qemu-backend` and the supervisor, with the watchdog; supervisor death with QEMU and swtpm running, no orphan.
5. Firmware: release hashes recorded; dbx content decided (offline guests receive no dbx updates); signature verification of the downloaded sources; cause of the stable202608 stall before any upgrade.

## References

Roadmap sections 4, 5, 7.2, 12 (R2, R2b, R2c). ADR-0003, ADR-0004, ADR-0005, ADR-0007, ADR-0010, ADR-0011, ADR-0015, ADR-0017, ADR-0027. `docs/verification/R2b-01-macos-hvf-windows11-arm64.md`.

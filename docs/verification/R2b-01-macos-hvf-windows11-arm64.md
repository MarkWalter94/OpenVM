# R2b-01 — macOS Apple Silicon host: HVF, Windows 11 ARM64 setup, firmware and process containment

- Execution date (UTC): 2026-10-12
- Executed by: development agent on the macOS host
- Roadmap step: section 12, R2 (increment R2b, macOS Apple Silicon); ADR-0028 (proposed)
- Related ADRs: ADR-0028 (proposed), ADR-0027, ADR-0015, ADR-0011, ADR-0010, ADR-0007, ADR-0004
- Overall outcome: partial. Verified: the Windows 11 ARM64 installer boots under HVF with a project-built Secure Boot firmware and an emulated TPM 2.0 and **passes the Windows 11 requirements check without any bypass**; process containment without orphans. Not executed: installation to disk, first boot from disk, OOBE, baseline/restore, the product command line.

## Purpose

The development host is an Apple Silicon Mac. Hypervisor.framework (HVF) accelerates only AArch64 guests, so the x86 Windows profiles of ADR-0027 cannot run accelerated on it, and the only Windows that can is Windows 11 ARM64. ADR-0027 excludes Windows 11 because it requires TPM 2.0 and Secure Boot and the roadmap forbids requirement bypasses. This spike checks whether those requirements can be met for real on this host, and records the first evidence for the macOS host layer (process containment).

What this test does NOT demonstrate: a working installation, the product command line (`-drive`/`-blockdev` mix, no supervisor), the console through the RFB client, the baseline of the TPM state. It does not update any cell of `docs/COMPATIBILITY.md`.

## Environment

| Item | Value |
| --- | --- |
| Host | Apple M3 Pro (11 cores), 18 GiB RAM |
| OS and build | macOS 27.0.1 (26A434) |
| Accelerator | HVF (`query-accelerators` → `enabled: hvf`) |
| QEMU binary | QEMU 11.1.2 from Homebrew, `qemu-system-aarch64` SHA-256 `7299ef807db462f913be5d1b89a71ea12590416175be0be5e87c2349594c6542`, signed with the `com.apple.security.hypervisor` entitlement. Not the build of ADR-0010 (Windows only) |
| TPM emulator | swtpm 0.10.2 (Homebrew), TPM 2.0, one state directory per VM |
| Guest media | `Win11_25H2_EnglishInternational_Arm64.iso` (user's copy), SHA-256 `f00ca08fd3b58024817b1ca2a91b8f299ce4f8be9b80bbf79de32c91e217d2bb`; setup build 10.0.26100.6584 |
| Toolchain for the firmware | Homebrew clang/LLVM 23.1.3, lld 23.1.3, acpica 20260408 |

## Procedure and results

### 1. Hardware profile under HVF

Command line of the spike (the key devices; full scripts in the session scratchpad, outside the repository):

```text
qemu-system-aarch64 -no-user-config -nodefaults -accel hvf -machine virt,gic-version=3 -cpu host -smp 4 -m 4096
  -drive if=pflash,format=raw,readonly=on,file=<code.fd> -drive if=pflash,format=raw,file=<per-VM vars.fd>
  -chardev socket,id=chrtpm,path=<run>/tpm.sock -tpmdev emulator,id=tpm0,chardev=chrtpm
  -device tpm-tis-device,tpmdev=tpm0,ppi=off
  -device ramfb -device qemu-xhci -device usb-kbd -device usb-tablet
  -device nvme,drive=disk0,serial=lv0 (qcow2, -blockdev)
  -device usb-bot,id=bot0 -device scsi-cd,bus=bot0.0,drive=cd0,bootindex=0 (ISO, read-only)
  -nic none -display none -qmp unix:<run>/qmp.sock,server=on,wait=off
```

| Item | Outcome | Evidence |
| --- | --- | --- |
| `tpm-tis-device` with the default PPI | **fails**: `HV_BAD_ARGUMENT (0xfae94003, at ../accel/hvf/hvf-all.c:123)` at start | QEMU stderr |
| `tpm-tis-device,ppi=off` | works; `query-tpm` reports `tpm-tis` with the emulator backend | QMP |
| ISO on `usb-storage` | Windows sees a 6968 MB RAW removable disk, setup stops at "A media driver your computer needs is missing" | `diskpart` in WinPE |
| ISO on `usb-bot` + `scsi-cd` | Windows mounts it as `D:` (UDF, CD-ROM), setup continues | `diskpart list volume` |
| NVMe disk | seen by WinPE (Disk 0, 64 GB) with the inbox driver | `diskpart list disk` |
| Display `ramfb`, 800×600 in setup | works (firmware GOP and Windows setup) | screenshots |
| USB keyboard and absolute USB tablet through QMP `input-send-event` | work (boot prompt, WinPE console, clicks in setup) | screenshots |
| No NIC | `-nic none`; `info network` empty | QMP |
| VNC on a Unix socket | RFB banner `RFB 003.008` | spike with an empty ISO |

### 2. Windows 11 requirements check

The setup was driven to the requirements check (language, keyboard, "Install Windows 11", "I don't have a product key" — the official deferred-activation path —, edition Windows 11 Pro).

| Firmware | Outcome |
| --- | --- |
| Homebrew `edk2-aarch64-code.fd` (edk2-stable202408 prebuilt by qemu.org, no Secure Boot) | "This PC doesn't currently meet Windows 11 system requirements — The PC must support Secure Boot." **Only** that item: TPM 2.0, CPU, RAM and storage pass |
| Debian `qemu-efi-aarch64` 2025.02-8+deb13u1 (`AAVMF_CODE.secboot.fd` + `AAVMF_VARS.ms.fd`; hash chain InRelease → Packages → .deb checked, InRelease signature not verified) | hangs at "Start boot option"; same with the `no-secboot` variant and without TPM. Not used |
| Project build of edk2-stable202608 (Secure Boot, TPM2) | synchronous exception in `Tcg2Pei` (see §3); with the patch, the BDS countdown never advances (`[Bds]BdsWait(5)` is the last line). Not used; cause of the timer stall not investigated |
| **Project build of edk2-stable202408 + patch 0001 (Secure Boot, TPM2, no network)**, vars with Secure Boot on | Windows boot manager (Microsoft-signed) starts with Secure Boot enforced; setup reaches the licence terms page, the step after the requirements check: **requirements met** |

The test stopped at the licence terms page: accepting the Microsoft licence is the user's act, and the free space on the host (14 GiB) is not enough for an installation.

### 3. Firmware build and the HVF fault

`packaging/firmware/edk2-aarch64/build.sh` (ADR-0028) builds edk2-stable202408 (`b158dad150bf02879668f72ce306445250838201`) `ArmVirtPkg/ArmVirtQemu.dsc`, `RELEASE`, `CLANGDWARF`, with `SECURE_BOOT_ENABLE=TRUE`, `TPM2_ENABLE=TRUE`, `NETWORK_ENABLE=FALSE`.

Cause found with a DEBUG build and the serial log: `Synchronous Exception at 0x6BBF4` (stable202608) / `0x5BAD8` (stable202408) in `Tcg2Pei`, resolved with `llvm-objdump`/`llvm-addr2line` to `BaseRngLibConstructor` (`MdePkg/Library/BaseRngLib/AArch64/Rndr.c:48`), instruction `strb w9, [x8, #0xdd0]`: the constructor caches RNDR support in a global, and PEIMs run in place from the read-only flash, where the store faults under HVF. Patch `0001-BaseRngLib-AArch64-no-writable-global-in-XIP.patch` (SHA-256 `1e45e0a4e0c928007b11c2a393ab585b7445a8655e39c2578fb4cded80ac653c`) reads `ID_AA64ISAR0_EL1` on every check instead. The qemu.org prebuilt (GCC) does not show the fault; why the GCC build is not affected was not investigated.

Secure Boot keys: `virt-fw-vars` (virt-firmware 26.9) with `--enroll-generate "LightVM Platform Key" --microsoft-kek all --microsoft-db win11 --secure-boot`: generated PK (private key discarded), Microsoft KEK CA 2011 and 2023, db with Microsoft Windows Production PCA 2011 and Windows UEFI CA 2023 only (no third-party UEFI CA, no option ROM CA), dbx with no revocations.

| File (manual build of this run) | Size | SHA-256 |
| --- | --- | --- |
| `lv202408-code.fd` (code, padded to 64 MiB) | 67 108 864 | `c7dddd042febd411acb2c680574f5ddef7ebd38d7f5ff8c69cb38aa580e7a95b` |
| `lv202408-vars-blank.fd` | 67 108 864 | `b3b855c5a80310168051164986855692d1bdb06e67619856177965cd87c6774f` (identical to Debian's empty `AAVMF_VARS.fd`) |
| `lv202408-vars-win.fd` | 67 108 864 | `306fba300b323739e8981245eb3f98a715c70972b716568efb72f2e49196c5e0` (contains a generated PK: differs at every generation) |

The code image embeds the build time ("built at …"): the build is not bit-for-bit reproducible, so a released firmware is pinned by the hash of the shipped file, not by rebuilding.

Script check: `build.sh` run from an empty work directory produced `lightvm-aarch64-code.fd` SHA-256 `f558a54d27872e70ef79d75cb926f5080e209e2bade7486706150075c36d1c86` (different from the manual build only by the embedded build time), the same blank vars hash, and a vars file with a new PK. With these files the setup again reached the licence terms page (requirements met).

### 4. Process containment on macOS

macOS has neither cgroups nor `PR_SET_PDEATHSIG`. Mechanism tested (script outside the repository): the supervisor starts a watchdog with a lifeline pipe; the watchdog starts the child in a new process group and waits with kqueue on `EVFILT_PROC/NOTE_EXIT` of the supervisor and on EOF of the lifeline; on either it sends `SIGKILL` to the group. The supervisor is killed with `SIGKILL`; the time until the child's group is gone is measured.

| Child | Cycles | Orphans | p50 | p95 | max |
| --- | --- | --- | --- | --- | --- |
| `/bin/sleep` | 30 | 0 | 2.99 ms | 3.04 ms | 3.05 ms |
| `qemu-system-aarch64 -accel hvf -machine virt -S` | 30 | 0 | 8.35 ms | 9.89 ms | 11.43 ms |

After the runs `pgrep` found no `qemu-system-aarch64` or test child. Note: on macOS `killpg(pgid, 0)` returns `EPERM` while only zombies remain in the group; the measurement treats that as terminated. Not tested: killing the watchdog itself, descendants that leave the group, swtpm in the same group.

## 5. Product path (2026-10-12, later the same day)

Ignored test `crates/lightvm-service/tests/macos_real.rs` (`LIGHTVM_E2E_QEMU_DIR=/opt/homebrew`, `LIGHTVM_E2E_FIRMWARE_DIR=<build.sh output>`, `LIGHTVM_E2E_WIN11_ARM64_ISO=<ISO>`; `cargo test -p lightvm-service --test macos_real -- --ignored --nocapture`), through the service, `qemu-backend` and the Unix host layer (process group + lifeline watchdog, 0700 private directories): passed in 166 s.

| Item | Outcome |
| --- | --- |
| Import of the 7.3 GB ISO into the catalog | 36 s; detected `Windows11Arm64`, build 26200 |
| Start through the service | running 36 s after the request (mostly the start-time hash of the medium); HVF verified with `virt-11.1`, GICv3, `-cpu host`; firmware and swtpm verified against the approved (development) hashes; swtpm in the run's process group |
| No NIC | `info network` and `query-pci` checked by the backend before the VM is reported running |
| Console | frame 995/1000 lit after 128 s; `screendump` through the product screenshot path shows Windows 11 Setup "Select language settings" |
| Emergency Stop | completed; no `qemu-system-aarch64` or `swtpm` left (`ps`, `pgrep`) |
| Disk | 64 GiB qcow2, 393 KB used on the host |
| Without media | firmware with TPM and Secure Boot reaches the EDK II shell and lists the NVMe disk |

Development approvals only: Homebrew QEMU and swtpm and the project firmware are accepted through built-in development entries and `approved-dev.sha256`, not approved for release. Baselines of TPM VMs are refused until the swtpm state is part of them.

## Pending (ADR-0028 required evidence)

- Installation to the NVMe disk, reboot from disk, OOBE **offline** (24H2/25H2 OOBE expects a network; a path without network and without bypass must be found for the editions offered, or the limit declared), ACPI shutdown.
- Baseline/restore with disk, vars and swtpm state returned to the baseline state; BitLocker/TPM sealing behaviour after restore.
- The product command line through `qemu-backend`, console through the RFB client, per-profile accelerator probe.
- Firmware: cause of the stable202608 timer stall; dbx content; signature verification of the downloaded sources.
- Linux ARM64 guest (the roadmap's R2b target): ISO `ubuntu-26.04.1-desktop-arm64.iso` (SHA-256 `c54d196489d3c867975fb3bbb72ca52ec2e137456e305481f81096304e4d2517`, matches `SHA256SUMS`) downloaded but removed from the host before the test.

# R6-05 — Spike: Windows 11 x64 guest with emulated TPM 2.0 (swtpm) and Secure Boot on Windows hosts

- Execution date (UTC): 2026-10-15
- Executed by: development agent on the project host
- Roadmap step: section 12, R6; R2c
- Related ADRs: ADR-0035 (Windows 11 x64 with emulated TPM), ADR-0028 (swtpm and Secure Boot on macOS), ADR-0027 (Windows guest profiles), ADR-0010 (QEMU build), ADR-0011 (firmware)
- Status: PARTIAL — product integration and Windows PE x64 TPM/Secure Boot/NV persistence verified on 2026-10-16 (§13); installed Windows, warm reboot and TPM-aware baselines/snapshots remain unqualified

## Purpose

ADR-0035 leaves to a feasibility spike: where `swtpm` for Windows comes from (existing build or project build), the transport between the Windows build of QEMU and `swtpm`, the Secure Boot firmware for x86_64, and the proof of a Windows 11 x64 installation with TPM 2.0 and Secure Boot under WHPX. This file records what was executed on the project host, what was established only from sources and documentation, and what remains unverified.

Sections 1–12 record the original 2026-10-15 spike. Section 13 records the
subsequent product implementation and new host evidence. No host TPM changes
or driver installation were performed.

## Environment

| Item | Value |
| --- | --- |
| Host | Windows 11 Pro 10.0.26200; Hyper-V/WHPX |
| QEMU | 11.1.0 (`v11.1.0-12130-ge470268ff4`), `lab/qemu/qemu-11.1.0` (ADR-0010) |
| Scratch data | `E:\OpenVM\lab\win11-spike\` (git-ignored by `/lab/`) |
| Limits applied to every guest | at most 4 vCPU and 4 GiB, process priority BelowNormal, one guest at a time |

## Findings

### 1. The approved QEMU build has no TPM support at all

Executed on the bundled `qemu-system-x86_64.exe`:

```
> qemu-system-x86_64.exe -tpmdev help
qemu-system-x86_64.exe: -tpmdev: invalid option
> qemu-system-x86_64.exe -device help | findstr tpm
(no output)
```

Cause, from the QEMU v11.1.0 sources (`meson.build` lines 222–224):

```
have_tpm = get_option('tpm') \
  .require(host_os != 'windows', error_message: 'TPM emulation only available on POSIX systems') \
  .allowed()
```

`CONFIG_TPM` gates both the backends (`backends/tpm/`) and the front-ends (`hw/tpm/`: `tpm-crb`, `tpm-tis`). No QEMU build for Windows from upstream sources can have a TPM device: not the weilnetz.de build of ADR-0010, not MSYS2's `mingw-w64-*-qemu`. The out-of-tree `mssim` backend proposed upstream in 2023 (patch series "tpm: add backend for mssim") is not merged in 11.1.0 and would be gated by the same option.

A second, independent obstacle is in `backends/tpm/tpm_emulator.c` (v11.1.0, lines 619–632): the emulator backend creates the data channel with `qemu_socketpair(AF_UNIX, …)` and passes one end to `swtpm` with `CMD_SET_DATAFD` through `qemu_chr_fe_set_msgfds` (`SCM_RIGHTS`). Windows `AF_UNIX` sockets do not support descriptor passing, so this path cannot work on Windows even if `CONFIG_TPM` were enabled.

Consequence: Windows 11 x64 with a real emulated TPM on a Windows host **requires a project-patched QEMU build**, not only a `swtpm` build. This contradicts the implicit assumption of ADR-0035 that the approved QEMU of ADR-0010 is kept.

### 2. No maintained Windows build of swtpm/libtpms exists

- `swtpm` upstream (github.com/stefanberger/swtpm) has no Windows release artefacts; it has build fixes for **Cygwin** (CHANGES: "Define `__USE_LINUX_IOCTL_DEFS` in header file (Cygwin)"; issue/PR #577 "swtpm: Call msync with length = 0 on Cygwin"). No native MinGW/MSVC port.
- Cygwin's package repository has no `swtpm` or `libtpms` package (cygwin.com package summaries return 404).
- MSYS2's package database has no `swtpm`, `libtpms` or `ms-tpm` package (search "tpm" on packages.msys2.org returns only `libnatpmp` and `uhttpmock`).
- No other Windows distribution with verifiable provenance was found.

So `swtpm` must be built by the project.

### 3. The Secure Boot OVMF shipped with QEMU cannot run under WHPX (SMM)

The approved build ships `share/edk2-x86_64-secure-code.fd` (3 653 632 bytes, SHA-256 `32807682a9e5c0e2d192ecb6077941d4be8bcfa6b9cd6e60830c3a9f48c0555a`). QEMU's `roms/edk2-build.config` (v11.1.0) builds it from edk2 `OvmfPkgX64.dsc` with `TPM2_ENABLE=TRUE`, `SECURE_BOOT_ENABLE=TRUE`, `SMM_REQUIRE=TRUE`, `BUILD_SHELL=FALSE`; its descriptor `50-edk2-x86_64-secure.json` declares `requires-smm`. No vars template with keys is shipped: the only x86 template is `edk2-i386-vars.fd` (the one of ADR-0011, empty key store).

Executed with the bundled QEMU (no TPM, no disk):

```
qemu-system-x86_64 -accel whpx -machine q35,smm=on,pflash0=fwcode,pflash1=fwvars \
  -global driver=cfi.pflash01,property=secure,value=on -cpu Skylake-Client-v4 -smp 2 -m 2048 \
  -nodefaults -nic none -device VGA -display none \
  -blockdev driver=file,node-name=fwcode,filename=.../edk2-x86_64-secure-code.fd,read-only=on \
  -blockdev driver=file,node-name=fwvars,filename=<copy of edk2-i386-vars.fd>
```

Result: QEMU exits at start with

```
qemu-system-x86_64.exe: System Management Mode not supported by this hypervisor.
```

WHPX in QEMU has no SMM emulation, so every SMM-requiring OVMF is excluded on this host. Distribution firmware does not help: Debian's `edk2` packaging builds Secure Boot only together with `SMM_REQUIRE=TRUE` (`OVMF_CODE_4M.secboot.fd`), and its `OVMF_CODE_4M.fd` has neither Secure Boot nor SMM (sources.debian.org, `debian/rules`, `OVMF_4M_NO_SECBOOT_FLAGS` / `OVMF_4M_SECBOOT_FLAGS`). The firmware therefore has to be a **project build of `OvmfPkgX64.dsc` with `SECURE_BOOT_ENABLE=TRUE`, `SMM_REQUIRE=FALSE`, `TPM2_ENABLE=TRUE`**, the same policy ADR-0028 already applies on macOS.

Security consequence (to be stated in ADR-0035): without SMM the UEFI variable store is a plain writable pflash; Secure Boot signature checks are enforced by the firmware at boot, but code running in the guest kernel can rewrite the variable store (PK/KEK/db/dbx) directly. Secure Boot then protects against unsigned boot components, not against a guest that is already compromised at ring 0. This is the same model as any OVMF without SMM and is acceptable for the Windows 11 requirement check, but it is weaker than a physical machine or Hyper-V.

### 4. swtpm + libtpms 0.10.2 build for Windows with Cygwin (no patches)

Chosen method: **Cygwin**, the only Windows target upstream maintains fixes for. A native MinGW/MSVC port was not attempted: `swtpm` relies on `fork`, Unix sockets, `poll` on mixed descriptors, `mmap`/`msync` and POSIX file locking, which Cygwin provides and MinGW does not. MSYS2's own POSIX runtime (`msys-2.0.dll`, a Cygwin fork) was rejected because the `msys` repository has no `json-glib` (required by swtpm), only the MinGW ones.

Toolchain (all user-local, no elevation, under `lab/win11-spike/`):

| Item | Value |
| --- | --- |
| Cygwin installer | `setup-x86_64.exe`, SHA-256 `2c9f2fb56e1fb687b5d9680afa8f8b06e6214f0e483096af0eae1946431226c5`; Authenticode **Valid** (CN=Jon Turney); detached GPG signature good, key `5640 5CF6 FCC8 1574 682A 5D56 1A69 8DE9 E2E5 6300` (Cygwin) |
| Install | `--quiet-mode --no-admin --root lab/win11-spike/cygwin64`, mirror `mirrors.kernel.org/sourceware/cygwin`; Cygwin 3.6.11, gcc 14.4.0; glib 2.90.0, json-glib 1.4.2, OpenSSL 3.5.9, GnuTLS 3.8.13, libtasn1 4.21.0 (setup verifies `setup.ini` with its built-in key) |
| libtpms | v0.10.2, GitHub archive SHA-256 `edac03680f8a4a1c5c1d609a10e3f41e1a129e38ff5158f0c8deaedc719fb127`; `v0.10.2.tar.gz.asc` good signature, key `B818 B9CA DF90 89C2 D5CE C66B 75AD 6580 2A0B 4211` (Stefan Berger); signed tag verified by GitHub (commit `03ff2481e133540be3b3ffe3daa1483d2a73d967`) |
| swtpm | v0.10.2, GitHub archive SHA-256 `f61cf6f1e9bbcb4cefb30b70cafaf1c4df54c6961e65cfa63830e8ad0e220134`; same signer; signed tag commit `32b5a0575991a73398de9749d63cafd847008af2` |

Build (`lab/win11-spike/scripts/cyg-build-tpm.sh`):

```
libtpms: ./autogen.sh --prefix=$PREFIX --with-openssl --with-tpm2 --disable-static && make && make install
swtpm:   PKG_CONFIG_PATH=$PREFIX/lib/pkgconfig ./autogen.sh --prefix=$PREFIX --with-gnutls --without-seccomp && make && make install
```

Both built **without any source patch**. Outputs: `cygtpms-0.dll`, `swtpm.exe`, `swtpm_ioctl.exe`, `swtpm_setup.exe`, `swtpm_cert.exe`, `swtpm_localca.exe`, `swtpm_bios.exe`. `swtpm socket --print-capabilities` reports version 0.10.2 with `tpm-2.0`, `flags-opt-startup`, `ctrl-opt-terminate`, `nvram-backend-dir`, `tpmstate-opt-lock`.

Runtime closure of `swtpm.exe` (cygcheck): `cygwin1.dll`, `cygtpms-0.dll`, `cygcrypto-3.dll`, `cygglib-2.0-0.dll`, `cyggio-2.0-0.dll`, `cyggobject-2.0-0.dll`, `cyggmodule-2.0-0.dll`, `cygjson-glib-1.0-0.dll`, `cygffi-8.dll`, `cygpcre2-8-0.dll`, `cygiconv-2.dll`, `cygintl-8.dll`, `cygz.dll`, `cyggcc_s-seh-1.dll` plus system DLLs. `swtpm_setup`/`swtpm_cert` additionally need GnuTLS/libtasn1 and are only needed to create an EK certificate (not required by Windows 11 setup).

Standalone test over loopback TCP (`scripts/cyg-swtpm-smoke.sh`, log `logs/swtpm-smoke.log`):

```
swtpm socket --tpm2 --tpmstate dir=<state> --ctrl type=tcp,port=24321,bindaddr=127.0.0.1 \
  --server type=tcp,port=24322,bindaddr=127.0.0.1 --flags not-need-init,startup-clear
swtpm_ioctl --tcp 127.0.0.1:24321 -c        -> ptm capability is 0x1efff
swtpm_ioctl --tcp 127.0.0.1:24321 --info 1  -> {"TPMSpecification":{"family":"2.0","level":0,"revision":183}}
TPM2_GetRandom(8) on the data port          -> rc 0, 8 random bytes
TPM2_GetCapability(TPM_PROPERTIES)          -> PT_FAMILY_INDICATOR "2.0", PT_MANUFACTURER "IBM"
swtpm_ioctl -s                              -> clean shutdown; state file tpm2-00.permall (4409 bytes)
```

Capability `0x1efff` has bit 12 (`PTM_CAP_SET_DATAFD`) **clear** when the data channel is a TCP server: consistent with the QEMU-side change of §5.

**Licences and redistribution.** libtpms: BSD-3-Clause (with parts under the TCG reference-code licence, also BSD-style); swtpm: BSD-3-Clause. Redistribution in binary form needs the licence texts in the distribution. The Cygwin runtime `cygwin1.dll` is **LGPL-3.0-or-later** (with the Cygwin linking exception for open-source programs); GLib, json-glib and libiconv/libintl are LGPL-2.1-or-later; OpenSSL 3 is Apache-2.0; PCRE2 BSD-3-Clause; zlib, libffi permissive. Shipping them requires the licence texts, source offer for the LGPL parts (Cygwin package source tarballs at the pinned versions) and allowing relinking (dynamic DLLs already satisfy this). Process-level consequence: `cygwin1.dll` keeps per-installation shared memory keyed by its own path; a product copy in its own directory does not interfere with a user's separate Cygwin, but two different `cygwin1.dll` versions loaded from the same directory would.


### 5. Patched QEMU 11.1.0 for Windows with the TPM emulator backend (built, works under WHPX)

Built on the project host in MSYS2 UCRT64 (gcc 16.2.0, meson 1.12.1; package list `packaging/qemu-win11/msys2-packages.txt` on branch `win11-qemu`) from the upstream tarball:

| Item | Value |
| --- | --- |
| `qemu-11.1.0.tar.xz` | SHA-256 `6ee1d1a61f68212476b27108c26da5f449dc09b626d42f8279ba0dc2e08fa858`; `gpg --verify` of `qemu-11.1.0.tar.xz.sig`: good signature, key `CEAC C9E1 5534 EBAB B82D 3FA0 3353 C9CE F108 B584` (Michael Roth) |
| MSYS2 base | `msys2-base-x86_64-20260927.tar.xz`, SHA-256 `ea2f31a0b6ade63914ce441ffb022f0f6aa96982bfefa2326460a26d5fb01322`; good signature, key `0EBF 782C 5D53 F7E5 FB02 A667 46BD 761F 7A49 B0EC` (Christoph Reiter); packages from the signed MSYS2 repositories |
| Configure | `--target-list=x86_64-softmmu --enable-whpx --enable-tpm --enable-gnutls` (no docs/SDL/GTK/SPICE/OpenGL/curl/libssh/libnfs/capstone/guest agent/tools); summary: `WHPX support: YES`, `TPM support: YES`, `GNUTLS support: YES 3.8.13` |
| Built target | `qemu-system-x86_64.exe` only (`ninja -j4`, BelowNormal); the unit tests are not built (one of them, `tests/unit/check-qjson.c`, stopped the first full `ninja` run; irrelevant to the distribution) |
| Bundle used for the tests | `lab/win11-spike/qemu-tpm/`: stripped `qemu-system-x86_64.exe` SHA-256 `3dbb37d1e30a868c7b1cdfbcc9bdf41ab2824b73550bdfaac9aee6ff9f6a7c19`, 25 MSYS2 DLLs, pc-bios blobs of the same tarball in `share/`; `SHA256SUMS` in the directory |

Patches (repository branch `win11-qemu`, `packaging/qemu-win11/`):

- `0001-tpm-allow-the-emulator-backend-on-Windows-hosts.patch` (meson.build, qapi/tpm.json, backends/tpm/tpm_emulator.c; about 130 added lines): `CONFIG_TPM` allowed on Windows; new optional `-tpmdev emulator,…,data-chardev=<id>`: the TPM commands use that chardev (TCP socket to `swtpm --server type=tcp`) instead of a socketpair passed with `CMD_SET_DATAFD`, and `PTM_CAP_SET_DATAFD` is not required then; the TPM version probe uses the emulator's own transport. Without `data-chardev` the POSIX behaviour is unchanged; on Windows it is an error.
- `0002-whpx-do-not-unmap-unaligned-sections.patch` (accel/whpx/whpx-common.c, one hunk). Found during this spike: with patch 0001 alone, `-device tpm-crb` aborted QEMU at start with `WHPX: failed to unmap GPA range`. Cause: the CRB command buffer is a RAM region at `0xFED40080` and the PPI RAM is 0x400 bytes, neither page-aligned; `whpx_set_phys_mem()` turns an unaligned RAM section into a `WHvUnmapGpaRange()` call, which WHPX rejects for an unaligned range, then calls `abort()`. The patch returns early for unaligned sections (they were never mapped); guest accesses exit to QEMU and are emulated. `tpm-tis` has the same PPI region and needs the same patch.

Executed with the bundle:

```
> qemu-system-x86_64 -tpmdev help        -> emulator   TPM emulator backend driver
> qemu-system-x86_64 -device help        -> name "tpm-crb"; name "tpm-tis", bus ISA
> qemu-system-x86_64 -accel help         -> tcg, whpx
```

The build is not bit-reproducible (paths, timestamps): binaries are approved by hash like the ADR-0010 build. Recipe re-run from an empty directory: §9.

### 6. QEMU and swtpm over two loopback TCP connections (verified)

`swtpm` (Cygwin build of §4) is started as a native Windows process by the launcher, then QEMU:

```
swtpm socket --tpm2 --tpmstate dir=/cygdrive/e/.../tpmstate \
  --ctrl type=tcp,port=24331,bindaddr=127.0.0.1,terminate \
  --server type=tcp,port=24332,bindaddr=127.0.0.1 --log file=.../swtpm.log,level=20
qemu-system-x86_64 -accel whpx -machine q35 ... \
  -chardev socket,id=chrtpm,host=127.0.0.1,port=24331 \
  -chardev socket,id=chrtpmdata,host=127.0.0.1,port=24332 \
  -tpmdev emulator,id=tpm0,chardev=chrtpm,data-chardev=chrtpmdata -device tpm-crb,tpmdev=tpm0
```

- QMP `query-tpm` returns `[{"model":"tpm-crb","options":{"type":"emulator","data":{"chardev":"chrtpm","data-chardev":"chrtpmdata"}},"id":"tpm0"}]`.
- swtpm log: QEMU's version probe `TPM2_ReadClock` on the data port (answer `TPM_RC_INITIALIZE`, tag `0x8001`, so TPM 2.0), `CMD_GET_CAPABILITY` on the ctrl port (`0x1efff`), then `CMD_INIT` and `SET_BUFFERSIZE`; with SeaBIOS: `TPM2_Startup`, `SelfTest`, `GetCapability`, `PCR_Extend`. Log: `lab/win11-spike/logs/r605-seabios-swtpm.log`.
- `Get-NetTCPConnection`: both ports `Established` between the swtpm PID and the QEMU PID only.
- Supervision: killing QEMU, or a guest poweroff (QEMU exits), makes swtpm exit by itself (`terminate`); its state file is written on the way out.

### 7. TPM 2.0 inside a guest, measured boot, persistence across restarts (verified with Linux)

No Windows 11 x64 ISO is present under `lab/media` (only Windows 10 22H2 x86/x64 and Ubuntu 26.04.1 desktop), so Windows 11 Setup was **not run**. The TPM was verified with the Ubuntu 26.04.1 live system (ISO hash-verified in `lab/media`), with kernel and initrd extracted from the ISO and a serial console, under WHPX (2 vCPU, 3 GiB, BelowNormal). The firmware was the approved **non-Secure-Boot** OVMF of ADR-0011 (`edk2-x86_64-code.fd`, built by QEMU with `TPM2_ENABLE=TRUE`), because the project Secure Boot OVMF could not be completed (§8). Command line: `lab/win11-spike/logs/r605-lin-cmdline.txt`.

In the guest (kernel 7.0.0-30-generic):

```
/dev/tpm0 (crw-rw---- tss root 10,224), /dev/tpmrm0
/sys/class/tpm/tpm0/tpm_version_major          -> 2
/sys/class/tpm/tpm0/device/description         -> TPM 2.0 Device
dmesg: efi: ... TPMFinalLog=0x7fbf7000 ... TPMEventLog=0x7fb70018
       ACPI: TPM2 0x000000007FB76000 00004C (v04 BOCHS BXPC ...)
/sys/kernel/security/tpm0/binary_bios_measurements -> 6113 bytes (firmware event log)
pcr-sha256/0 = 1DC5C72501E65FD8166213E3D9BE55B0ED30680F0D95AE2DD2B04038AC5AFA5E
pcr-sha256/7 = B5710BF57D25623E4019027DA116821FA99F5C81E9E38B87671CC574F9281439
mokutil --sb-state -> This system doesn't support Secure Boot   (expected with this OVMF)
```

Persistence (script `lab/win11-spike/scripts/nv.py`, raw TPM2 commands on `/dev/tpmrm0`, password session with empty auth):

1. Boot 1: `TPM2_NV_DefineSpace` of index `0x01500016` (16 bytes, AUTHREAD|AUTHWRITE), `TPM2_NV_Write` of "LightVM-R605-ok.", `TPM2_NV_Read`: rc 0, data returned.
2. `sudo poweroff`: QEMU exits, swtpm exits (`terminate`), `tpm2-00.permall` rewritten (1370 bytes).
3. New swtpm and new QEMU on the same state directory and vars file. Boot 2: `TPM2_NV_Read` returns rc 0 and `b'LightVM-R605-ok.'`. PCR0 and PCR7 are identical to boot 1.

Result: the TPM state persists across a full stop/start of both processes. Logs: `logs/r605-lin-swtpm-boot1.log`, `logs/r605-lin-swtpm-boot2.log`.

Warm reset (`system_reset` over QMP): the TPM side behaves (QEMU sends `CMD_STOP`, `SET_BUFFERSIZE` and `CMD_INIT` on the ctrl channel, all rc 0), but the guest then stops with `failed to get xsave state: Input/output error` and `WHPX: Unexpected VP exit code 4`. **The same failure occurs with the approved upstream QEMU 11.1.0 build and no TPM** (same firmware, kernel and CPU model), so it is a pre-existing WHPX reset problem, not caused by the patches. It matters for Windows 11, whose Setup reboots several times: a guest-initiated reboot was not tested in this spike. Until it is checked, the product should not use QMP `system_reset` for this profile.

Not verified: baseline/snapshot revert of the TPM state (only file-level persistence), and TPM-sealed secrets (BitLocker, Windows Hello) after a revert.

### 8. Project OVMF with Secure Boot and without SMM: build blocked on this host by the antivirus

Recipe on branch `win11-qemu`, `packaging/firmware/edk2-x64/` (`build.ps1`, `gen-pk.py`, one edk2 patch): edk2-stable202408 (`b158dad150bf02879668f72ce306445250838201`, checked with `git rev-parse`), `OvmfPkgX64.dsc`, `RELEASE`, toolchain tag `VS2019` with Visual Studio 2022 Build Tools (MSVC 14.44), NASM 3.02 (MSYS2), ACPICA `iasl` 20260408 (`iasl-win-20260408.zip`, SHA-256 `121f5e4f30b1df63d09052294e4a605d4dee2dfb9599fa24af4ac6015df02b70`); `-D SECURE_BOOT_ENABLE=TRUE -D SMM_REQUIRE=FALSE -D TPM2_ENABLE=TRUE -D TPM2_CONFIG_ENABLE=TRUE -D TPM1_ENABLE=FALSE -D BUILD_SHELL=FALSE`.

Findings:

1. With MSVC 14.44, `DisplayEngineDxe` fails to link (`LNK2001: memcpy`): the compiler lowers a structure copy to `memcpy`, which edk2 does not provide to that module. Fixed by `0001-OvmfPkgX64-IntrinsicLib-for-DisplayEngineDxe-MSVC.patch`, which links `CryptoPkg/Library/IntrinsicLib` into that module only (a DSC-scoped change).
2. An earlier build attempt had been interrupted mid-write and had left a truncated `OpensslLibCrypto.lib` (exactly 2 MiB), causing unresolved OpenSSL symbols in `Hash2DxeCrypto`; a clean build fixed it.
3. With the clean build, **all modules compile and link**. Firmware volume assembly then fails with `"LzmaCompress" is not recognized`. `nmake` links the BaseTools host tool `E:\OpenVM\lab\win11-spike\build\edk2\BaseTools\Bin\Win32\LzmaCompress.exe` successfully, and the file **disappears within 15 seconds**. This happened twice; its `.pdb` and the other BaseTools `.exe` files remain. The host runs a corporate endpoint protection product ("Athena EPP Antivirus", service `savsvc`) alongside Windows Defender, and Defender's operational log shows no detection. The user confirmed that this antivirus is quarantining files under `E:\OpenVM\lab`. Following the owner's instruction, the step was stopped there: no attempt to evade or bypass the antivirus, no alternative compressor, no WSL2 rebuild. As a result, `OVMF_CODE.fd` and `OVMF_VARS.fd` were **not produced**. Booting with Secure Boot, the Windows 11 requirement check and the Secure Boot state in a guest are **not verified**.
4. Key enrolment was verified separately, on the OVMF variable-store template `edk2-i386-vars.fd` of the approved build, with virt-firmware 26.9 installed with `--require-hashes` (wheel SHA-256 `64d0078e6dd0c82a1df3a97c3cc8a149340068095cf8cfb6beb02005e3194160`). `--enroll-generate` fails on Windows (`PermissionError` when reopening a `NamedTemporaryFile`), so `gen-pk.py` generates the PK certificate with `cryptography` and discards the key. `--enroll-cert pk.der --microsoft-kek all --microsoft-db win11 --secure-boot` then produces: PK = `CN=LightVM Platform Key`; KEK = LightVM PK cert, `Microsoft Corporation KEK CA 2011`, `Microsoft Corporation KEK 2K CA 2023`; db = `Microsoft Windows Production PCA 2011`, `Windows UEFI CA 2023`; `SecureBootEnable` on, `CustomMode` off; dbx = the template's 76-byte placeholder (no revocation entries). This is the db/KEK policy of ADR-0028.

Files observed missing (quarantined) during this session: `E:\OpenVM\lab\win11-spike\build\edk2\BaseTools\Bin\Win32\LzmaCompress.exe` (twice, 15:05 and 15:12 local time). Checked present: `qemu-tpm\qemu-system-x86_64.exe` and its DLLs (match `SHA256SUMS`), `swtpm-prefix\bin\swtpm.exe`, `swtpm_ioctl.exe`, `swtpm_setup.exe`, `cygtpms-0.dll`, `cygwin64\bin\cygwin1.dll`, MSYS2 `gcc.exe`/`python.exe`/`bash.exe`, `tools\iasl\iasl.exe`, and the other BaseTools executables.

### 9. Build recipe re-run from an empty directory (verified)

`packaging/qemu-win11/build-qemu.sh` (branch `win11-qemu`) was run in MSYS2 UCRT64 (`-j4`, BelowNormal) on a new work directory holding only the tarball and its `.sig`. It checked the SHA-256 and the `VALIDSIG` of key `CEACC9E15534EBABB82D3FA03353C9CEF108B584`, extracted the sources, applied both patches cleanly (`patching file meson.build`, `qapi/tpm.json`, `backends/tpm/tpm_emulator.c`, `accel/whpx/whpx-common.c`), configured (`WHPX support: YES`, `TPM support: YES`), built 1715 targets and assembled the bundle: `qemu-system-x86_64.exe` (stripped) SHA-256 `78c864e2cc65f80187a77eefa7e7c35e8e9ccbefd5e9c4e31e62172603b34f49`, 25 DLLs, `share/` blobs and licence files, `SHA256SUMS` (log `lab/win11-spike/logs/qemu-repro.log`). The executable hash differs from the one in §5 (`3dbb37d1…`), as expected for a non-reproducible build. The rebuilt binary passed the same smoke test: `-tpmdev help` lists `emulator`; with swtpm over TCP and SeaBIOS, `query-tpm` reports `tpm-crb`/`emulator` with `data-chardev`, swtpm logs 20 TPM commands, and swtpm exits when QEMU is stopped.

Recipe fix found by this run: MSYS2 `tar` emulates symlinks by copying their target and fails while the target is not extracted yet (`roms/`, `tests/lcitool/`, `rust/`, `subprojects/`). The script now excludes `roms/` and `tests/lcitool/` (not needed) and extracts twice, and the second pass must succeed.

Lab note: the spike's QMP helper gets an answer only on the first QMP connection to a QEMU process (observed with a `pipe:` and with a `tcp:` QMP endpoint, on the patched build with and without a TPM device; not checked on the upstream build); later connections time out. This does not concern the product, which keeps its own QMP channel of ADR-0012, but the lab helper should keep a single connection open.

### 10. Loopback exposure of the swtpm ports (verified)

`swtpm` TCP channels have no authentication. Measured with a standalone swtpm (`scripts/second-client.py`): while a first client (in QEMU's role) holds **both** the ctrl and the data connection, a second local client can open a TCP connection to either port (it sits in the listen backlog) but receives **no answer** on either (3 s timeouts). If the first client holds only the data port, a second client is served on the ctrl port (`CMD_GET_CAPABILITY` answered). When the first client closes the ctrl connection, swtpm exits (`terminate`), so a queued client is never served afterwards. The exposure is therefore the window between swtpm's `listen()` and QEMU's two `connect()` calls, open to any local process of any user (loopback is not per-user).

A Unix socket is not an alternative here: Cygwin's `AF_UNIX` is its own emulation (a socket file plus a secret handshake over loopback TCP) and does not interoperate with the native Windows `AF_UNIX` used by QEMU. Passing pre-connected descriptors (`--ctrl type=unixio,clientfd=…`) from a native parent into a Cygwin child is not available either.

### 11. What the product implementation needs

**Profile `Windows 11 x64`** (ADR-0035): `pc-q35-11.1`, `-accel whpx`, CPU model of ADR-0011 (`Skylake-Client-v4`, host/max excluded), at least 2 vCPU and 4096 MiB (the Windows 11 minimum is also the lab guest limit), disk at least 64 GiB on AHCI; `pflash0` = project `lightvm-x64-code.fd` read-only, `pflash1` = per-VM copy of `lightvm-x64-vars-win.fd`; `-chardev socket` ×2 + `-tpmdev emulator,chardev=…,data-chardev=…` + `-device tpm-crb`. The profile selects the **project QEMU bundle**, installed in its own directory next to (not replacing) the upstream build of ADR-0010; every other profile keeps the upstream binary. QMP and display keep the private channels of ADR-0012/0013 (only the TPM uses TCP). Until the warm-reset failure of §7 is understood, do not issue QMP `system_reset` for this profile; test guest-initiated reboots during Setup.

**swtpm process supervision** (in the VM's process tree, like QEMU):
1. Choose two free loopback ports for this run (bind-probe), start `swtpm socket --tpm2 --tpmstate dir=<vm>/tpm,lock --ctrl type=tcp,port=P1,bindaddr=127.0.0.1,terminate --server type=tcp,port=P2,bindaddr=127.0.0.1 --log file=<vm>/logs/swtpm.log,level=1` with BelowNormal priority, in the same Windows job object as QEMU (kill-on-close), with a minimal environment (`PATH` = the swtpm directory only, so the bundled `cygwin1.dll` is the one loaded). On port collision (`bind` failure), retry with new ports.
2. Wait until both ports accept (bounded), then start QEMU immediately (this keeps the window of §10 short).
3. **Per-run binding check instead of a credential** (swtpm has none): right after QEMU has started, read the TCP table (`GetExtendedTcpTable`, owner PID) and require exactly one established connection to each swtpm port, whose peer PID is the QEMU process of this run; anything else (an extra established peer, a foreign PID) stops the VM and reports it. Record the ports and PIDs in the run log.
4. Stop: QEMU exit (guest poweroff, `quit`) makes swtpm exit by itself; the supervisor waits for it (bounded), then kills it if needed. If swtpm dies while QEMU runs, the guest's TPM fails: treat it as a VM failure and stop QEMU.
5. Path conversion for Cygwin (`E:\…` to `/cygdrive/e/…`), tested for spaces and non-ASCII characters in the catalog path (not tested in this spike).

**State per VM in the catalog**: `<vm>/tpm/` (`tpm2-00.permall`, about 1.3–4.4 KiB, plus the lock file) beside the per-VM vars file. It is created on first start (no `swtpm_setup` needed for Windows 11 Setup; an EK certificate is optional and would need `swtpm_setup`/`swtpm_localca` with GnuTLS). Baselines and snapshots (ADR-0005/0028) must contain disk + vars + `tpm/`, captured and restored only with the VM stopped (swtpm writes the state file on exit). Live snapshots would need the TPM state blobs through QEMU migration support, not verified. Deleting a VM deletes its TPM state; cloning a VM must reset the TPM state and the vars (new identity) or copy them knowingly. Consequence to document: secrets sealed to the TPM (BitLocker, Windows Hello PIN) follow the baseline in which they were created.

**Approved hashes** (verified like the ADR-0010 files before each start; a different file prevents the start): the project QEMU bundle (`qemu-system-x86_64.exe` + 25 MSYS2 DLLs + `share/` blobs, from the `SHA256SUMS` of the released build); `swtpm.exe`, `cygtpms-0.dll` and the 13 Cygwin runtime DLLs of §4 (`cygwin1.dll`, OpenSSL, GLib, json-glib and dependencies); `lightvm-x64-code.fd` and `lightvm-x64-vars-win.fd`. None of these builds is bit-reproducible: the hashes of the released files are the identity, and the recipes in `packaging/qemu-win11/` and `packaging/firmware/edk2-x64/` record how they were made.

**Packaging and licences**: the QEMU bundle (GPL-2.0, MSYS2 libraries LGPL/GPL-with-exception/BSD/MIT) and the swtpm bundle (BSD-3-Clause; `cygwin1.dll` LGPL-3.0+; GLib/json-glib/libiconv/libintl LGPL-2.1+; OpenSSL Apache-2.0) each in their own directory, with licence texts and a written source offer covering the QEMU tarball + patches, the MSYS2 and Cygwin source packages at the pinned versions, swtpm/libtpms 0.10.2 and edk2-stable202408 + patch. The swtpm directory must contain exactly one `cygwin1.dll`.

**Code signing**: on this host the corporate antivirus quarantined an unsigned freshly built executable (§8). Project-built QEMU, swtpm and DLL bundles distributed unsigned are exposed to the same behaviour on users' machines; the distribution should sign them (Authenticode) or document the exclusion an administrator must grant, and the diagnostics should report a missing or quarantined binary by its hash check.

### 12. Status of the required evidence of ADR-0035

| Evidence | Status |
| --- | --- |
| swtpm/libtpms build reproducible (Cygwin, pinned, signature-verified) | verified (§4) |
| QEMU for Windows with a TPM device under WHPX, recipe re-run | verified (§5, §9) |
| TPM 2.0 reported by a guest, measured boot | verified with Linux (§7) |
| TPM state survives a restart | verified with Linux, cold restart (§7) |
| TPM state after baseline and snapshot revert | not verified |
| Project OVMF with Secure Boot, without SMM | recipe written, build blocked by the host antivirus (§8); key enrolment verified |
| Windows 11 x64 Setup passes the requirement check and installs, Secure Boot on | not verified (no Windows 11 ISO in `lab/media`; firmware not built) |
| Warm reset under WHPX | fails with the upstream build too (§7), to be investigated |

## 13. Product integration and real Windows PE verification (2026-10-16)

The owner deferred physical USB testing and asked to proceed with Windows TPM.
All build trees, downloads, outputs and VM scratch are on E: under
`E:\OpenVM-tests\win11` and `E:\OpenVM-tests\lab\win11-real`.
No installed Windows VM, host TPM, host driver or requirement bypass was used.

### Product changes

- `windows11_x64` is a distinct guest in the protocol, ISO detector, core and
  desktop wizard. It uses q35/AHCI/VGA, xHCI tablet, UEFI Secure Boot and TPM
  CRB, requires at least two vCPUs, 4096 MiB and 64 GiB, and is currently
  Windows-host-only. Firmware/hardware stay fixed after creation. TPM-bearing
  VMware imports remain refused instead of losing the source TPM identity.
- The supervisor selects the separate project bundle only for this guest.
  `--win11-dir` selects it explicitly; default: `<ordinary-qemu-dir>/win11`.
  Development approvals accept `<manifest-sha256> win11-bundle`. The complete
  top-level `SHA256SUMS` must be approved; all listed files are hashed, links,
  missing/changed/unlisted files are refused, and write/delete-excluding file
  handles are held for the run. The ordinary QEMU, firmware and capability
  cache remain in use for other profiles. There are no release bundle hashes
  approved by default yet.
- The TPM process starts in the same kill-on-close Job Object as QEMU,
  with a minimal environment and PATH limited to its own DLL directory.
  Cygwin paths are constructed from validated native drive paths. Both
  loopback listeners are checked by owning PID without opening probe
  connections. After QEMU starts, the kernel TCP table must show exactly one
  connection per endpoint with the expected server and QEMU PIDs. Missing
  peers time out; foreign/additional peers refuse start before vCPUs run.
  This remains an unauthenticated loopback protocol, with the initial
  listen/connect window described in §10; PID inspection is not a credential.
- Before guest execution, QMP must report exactly one `tpm-crb` backed by
  `emulator` (`tpm-tis` on the existing ARM profile). A helper lifeline stops
  the entire run if swtpm dies. Normal exit, forced stop and close release
  the helper, QEMU, process handles and Job Object. Persistent state is under
  the existing per-VM catalog `tpm/` directory; it is reused on cold restart.
- Windows DLLs were found creating relative Known Folder caches in the
  binary directory with a minimal environment. Managed process cwd now uses
  its run-output directory; help/version probes use the configured temp
  directory. This prevents cache writes from invalidating the approved
  bundle. Lab temp/run directories are on E:.
- At this checkpoint, baselines and user snapshots of TPM guests were
  refused: copying only the disk/UEFI variables would roll them back
  without the TPM. §14 adds the TPM state to both. The no-SMM firmware
  limitation of §3 remains.

### Builds and provenance

The QEMU 11.1.0 archive hash and Michael Roth signature were verified by the
existing recipe; the two existing patches applied and the WHPX/TPM build
completed with MSYS2 UCRT64 on E:. Its VNC smoke test exposed a missing runtime
asset: the recipe now includes `pc-bios/keymaps` and covers it in the manifest.
The recipe requires fresh source/empty output and no longer recursively
deletes existing source/output directories.

The Secure Boot firmware built successfully on this host with MSVC, pinned
edk2-stable202408 and the existing IntrinsicLib patch. The script now handles
MSYS2 Python's `bin/python.exe` venv layout and checks pip/key-generation
failures. Microsoft KEK 2011/2023 and Windows signing certificates were
enrolled; the platform private key was discarded by the existing generator.
`swtpm-windows/build.sh` verifies the pinned libtpms/swtpm 0.10.2 archives and
Stefan Berger signature, builds in user-local Cygwin and copies the DLL
closure. Upstream swtpm tests are disabled in this build recipe; product and
native guest tests provide the evidence below.

Development artifacts, not release approvals:

| File | SHA-256 |
| --- | --- |
| Project QEMU executable | `54fe68ee9d07f2a0c2709e65469202a1133b0db4421996d17dea6747b7009301` |
| swtpm executable | `350e2698d00d3261ca1de550cbb51a75c44c8c0c903fea1cf7e1072a81318572` |
| Secure Boot code | `939bc01ba166b849f5296457b2658fff78fb622c8dc34cfa8204ba3ce2d88d2c` |
| Enrolled variables template | `a1003e1c5fa5e776b35392745f878f4a4bb38b611699e03ec3fce4a682e62a4f` |
| Assembled `bundle-vnc/SHA256SUMS` | `bf10ebb1bee2c27b2b53d3fbd5b168c621f213239d4043cb18d31c6760d499bf` |

The Cygwin installer was Authenticode-valid (Jon Turney), with the same
SHA-256 as §4. The complete licence texts, runtime source inventory and
signing are still required before distribution; the assembled bundle is
marked DEVELOPMENT and has no automatic global approval.

### Real guest evidence

`windows11_real` uses the production bundle verifier, command builder,
QemuRun/Job Object lifecycle, authenticated Tools and isolated guest network.
The guest is disposable **Windows PE x64 from the owner's Windows 11 ISO**,
not an installed Windows system. A lab-only x64 Tools binary returns the
native probe log; no host clipboard, host TPM, installation licence prompt,
disk partitioning or Windows installation is automated.

The native guest probe reads SecureBoot/SetupMode firmware variables and
uses the Windows TBS API to identify TPM 2.0. It writes `LightVM2` into a
test-only NV index (`0x01500020`) in the disposable emulator, shuts the VM
down via QMP quit, then reads the same value on a cold start with a new run
identity. The second cycle kills only the identified, run-owned swtpm and
requires QEMU to stop within five seconds. Close must report zero active
processes, released process handles and a closed Job Object.

```text
secure_boot=1
setup_mode=0
tbs_status=0
tpm_version=2
persistent_nv=created  # first boot
persistent_nv=matched  # cold restart
```

The extended test passed in 63.42 s; its initial artifact directory is
`E:\OpenVM-tests\lab\win11-real\01a144dd-fae8-74b1-a7f4-6bd5a5bab6ec`.
A second ignored test starts the real service backend with Windows 11 twice,
checks per-VM TPM state, revalidates the unchanged bundle and starts/stops an
ordinary Linux profile afterwards. The final repeated artifacts, using the
fresh `build-winpe-test.ps1` outputs, are recorded in
`01a144eb-4a3a-75a7-8b02-dd760b797d92` (native guest) and
`backend-01a144eb-22e3-70b2-813d-bd80bfe418b4` (service backend) under
`E:\OpenVM-tests\lab\win11-real`. Both passed together in 76.68 s; the output
is in `E:\OpenVM-tests\win11\real-test-final.log`. After adding an explicit
startup-failure Job Object guard, the backend test passed again (9.95 s), in
`backend-01a144ef-c32a-743a-a475-12d5ec0550de`.

Additional checks cover real kernel-table listener/peer ownership (including
wrong PID and an extra connection), manifest/DLL tampering and unlisted files,
traversal, minimum CPU/memory/disk and firmware refusals, actual Windows 11 ISO
detection, schemas, core/service regression, desktop wizard and visual
snapshots. Installed Windows 11 Setup/OOBE, guest-initiated warm reboot,
BitLocker/Hello and TPM-consistent baseline/snapshot restore remain open.

Reproduction on the Windows lab host (MSVC environment loaded; all scratch
on E:), after building/assembling the explicit development bundle:

```powershell
packaging/tools/build-winpe-test.ps1 -Cc <native-x64-gcc.exe> -Out E:\OpenVM-tests\win11\new-winpe-fixture
$env:TEMP = 'E:\OpenVM-tests\tmp'
$env:TMP = $env:TEMP
$env:CARGO_TARGET_DIR = 'E:\OpenVM-tests\target'
$env:LIGHTVM_LAB = 'E:\OpenVM-tests\lab'
$env:LIGHTVM_WIN11_BUNDLE = 'E:\OpenVM-tests\win11\bundle-vnc'
$env:LIGHTVM_WINPE_FIXTURE = 'E:\OpenVM-tests\win11\new-winpe-fixture'
$env:LIGHTVM_WINPE_ISO = 'E:\OpenVM-tests\media\Windows11_Client_x64_it-it_26300_9457.iso'
cargo test -p lightvm-service --features supervisor --test windows11_real --locked -- --ignored --nocapture --test-threads=1
```

Run the test probes only in the disposable guest. The test writes its final
`results.txt`, two native guest logs and diagnostic PNGs into the new lab
directory. The PNG is a progress diagnostic, not proof of installed Windows;
the native API log is the TPM/Secure Boot evidence. Product/default tests do
not auto-approve or run the lab bundle.

Final focused checks passed: protocol/detector/core/fixture test suites,
99 QEMU backend library tests, 74 supervisor library tests plus Tools,
Windows-guest and snapshot integrations, 119 desktop library tests plus
wizard/transfer/settings UI tests, and three wizard visual tests (four
updated system-step PNGs, visually inspected). The OS choices were compacted
to keep navigation visible at normal window heights. All-targets clippy with
warnings denied, formatting, dependency advisories/licences/sources and
`git diff --check` pass. This is not a complete release qualification.

## 14. TPM-consistent baselines and snapshots; installation attempt (2026-10-16)

### Product changes

- A baseline or user snapshot of a TPM guest, taken with the VM stopped,
  now copies the swtpm permanent state together with the disk and UEFI
  variables. The copy is hashed through a guarded handle and recorded as a
  `tpm` firmware record of the baseline/snapshot; restore and revert verify
  the hash and install disk, variables and TPM state through the existing
  operation journal (`SnapshotRevertTpmRemoved` is the new intermediate
  step). An interrupted operation rolls forward or back as a whole, so disk
  and TPM never come from different points in time.
- Changed, missing or extra TPM state, and a baseline whose TPM presence does
  not match the VM, are refused. **A running snapshot of a TPM guest is
  refused**: QEMU's disk snapshot does not include the swtpm state.
- Storage tests cover capture, restore, revert, deletion and simulated
  interruptions at every journal step.

### Real Windows PE evidence

`windows_pe_tpm_state_follows_snapshot_and_baseline_restore` (ignored lab
test, same setup and reproduction as §13) uses the production catalog
operations and the real swtpm. In the disposable Windows PE guest the probe
writes a test NV value, the VM is stopped and a snapshot and a baseline are
taken; the probe then changes the value, and after each revert/restore the
next cold boot must read the original value:

```text
persistent_nv=created
persistent_nv=mutated
persistent_nv=matched   # after snapshot revert
persistent_nv=mutated
persistent_nv=matched   # after baseline restore
test result: ok. 1 passed; finished in 158.95s
```

Artifacts: `E:\OpenVM-tests\lab\w11\b31c5cf10a37\` (`checkpoint-results.txt`).
This is Windows PE with a test NV index, not BitLocker or Windows Hello in an
installed system.

### Installed Windows 11: first attempt (not passed)

`windows11_installs_and_reboots_with_tpm_and_secure_boot` (ignored lab test)
boots the owner's Windows 11 ISO with an answer file that only selects the
disposable 64 GiB disk on E:, with no requirement bypass. Setup passed its
requirement checks (TPM 2.0, Secure Boot) and copied files. **At its first
guest-initiated reboot QEMU logged `failed to get xsave state` ten times,
then `WHPX: Unexpected VP exit code 4`**, and the screen stayed black. A
resumed boot of the same disk failed the same way; the test hit its 40
minute Tools timeout without any vCPU fault event being reported to it.
Artifacts: `E:\OpenVM-tests\lab\win11-real\installed-01a1450c-537e-717b-832a-650f7377c392\`
(`run\qemu.stderr.log`, `setup.png`), log
`E:\OpenVM-tests\win11\install-resume.log`.

§15 records the causes and the qualification that followed.

## 15. Installed Windows 11 qualified: xsave fix and cold reboots (2026-10-16/17)

### Findings

1. **Stale xsave write-back.** With the 11.1.0 WHPX code QEMU logged
   `failed to get xsave state: No error` (12 times in the earlier Windows PE
   runs, 10 at the installation's first reboot), ignored the error and could
   later write its old xsave image back over the live vector registers.
   Patch `0003` (packaging/qemu-win11) checks the HRESULT, retries once with
   the size the hypervisor reports, frees the buffer and never writes back an
   image that was not read. In every run after it, no read failure and no
   retry was logged; why the original reads failed is not established.
2. **The vCPU fault watcher never fired under WHPX.** QEMU 11.1.0 stops a
   guest after `Unexpected VP exit code` in run state `paused`, not
   `internal-error`. Fixed in `qemu_backend::vcpu_fault` (R2c-01,
   correction of 2026-10-16); the automatic resets then appeared in the logs.
3. **A reset inside the running QEMU does not work for this profile.** A
   lab-only diagnostic build printed the state at the fault: RIP
   `0xfffcc704` in the firmware's SEC phase, empty IDT, CR3 `0x800000`, and
   two vCPUs (0 and 2) at the same RIP and stack, so the application
   processors ran the reset vector together with the BSP. Putting them back
   in wait-for-SIPI removed the triple fault, and recreating every virtual
   processor on reset gave them a cold-start state, but Windows Boot
   Manager still reset the machine immediately after loading, in a loop,
   while a cold start of the same disk worked. Windows PE reproduced it in
   4 minutes (`windows_pe_boots_again_after_a_warm_reset_with_the_tpm`). The
   persistent swtpm process is the main state a cold start renews; the
   same symptom is reported for Windows 11 with swtpm on other hypervisors
   ([UTM #7648](https://github.com/utmapp/UTM/issues/7648)). The exact
   cause inside Boot Manager was not determined.

### Product change: a reset is a cold reboot

- The Windows 11 x64 command line carries `-action reboot=shutdown`: every
  reset request (the guest's reboot, QMP `system_reset` from the Reset
  action or from the vCPU fault watcher) ends QEMU with a `SHUTDOWN` event
  whose reason is `guest-reset` or `host-qmp-system-reset`
  (`ExitCause::Reboot`).
- The service backend (`cold_reboot`) then closes the run's processes and
  starts **the same run** again with new QEMU and swtpm processes on the
  same disk, UEFI variables and TPM state. The network is attached again
  with the endpoints last applied; the Tools of the run stay and their old
  connections are closed (`ToolsRun::guest_restarted`). The core sees no
  exit: the VM stays running in the same run. At most 10 cold reboots in
  5 minutes; the next reset ends the run as an ordinary exit.
- The console connection of the previous process drops on a reboot; the
  desktop reconnects a dropped display after 2 s with a new ticket for the
  same run (`sync_console`). A new ticket for the run after a cold reboot
  shows the new process (§17).
- Other profiles keep resetting inside QEMU (upstream build).

### Evidence

All on Windows 11 Pro host build 26300, WHPX, the development bundle with
patches 0001-0003 (lab bundle `E:\OpenVM-tests\win11\bundle-xsave`), the
owner's Windows 11 ISO, disposable disks on E:.

| Test (ignored, lab) | Result |
| --- | --- |
| `windows_pe_boots_again_after_a_warm_reset_with_the_tpm` | passed (50 s): QMP reset → `ExitCause::Reboot` → cold start; Windows PE back, `tpm_version=2`, `tbs_status=0`, Secure Boot on, test NV value `matched` |
| `windows11_installs_and_reboots_with_tpm_and_secure_boot` | passed (584 s): fresh 64 GiB disk, Setup with its requirement checks (no bypass), 2 Setup reboots and 1 reboot requested by the installed system, all as cold reboots; Windows 11 Pro 10.0.26300 in audit mode; TPM 2.0 present/ready/owned, `Confirm-SecureBootUEFI` true; Tools reconnected and `LastBootUpTime` changed after `shutdown /r` |
| `service_backend_starts_windows11_twice_and_keeps_other_profiles_working` | passed (26 s): the service backend turns the Reset action into a cold reboot of the same run (no exit reported for 15 s, screenshot of the same run afterwards), then the earlier checks |

```text
cold reboot 1 at 254.9s
cold reboot 2 at 380.6s
Caption        : Microsoft Windows 11 Pro
Version        : 10.0.26300
guest reboot: cold reboot 3
test result: ok. 1 passed; finished in 584.15s
```

Artifacts (the disk of this first run was removed for space; its logs are
kept): `E:\OpenVM-tests\lab\win11-real\evidence-01a14712\`
(`results.txt`, `cold-reboots.txt`, `installed-before.log`,
`installed-after.log`, `tpm-0.log`, screenshots, `run\`) and
`E:\OpenVM-tests\lab\win11-real\backend-01a1471c-1dd4-73a1-b124-4ad28f2dbcd4\`.

### The same tests with the bundle built by the recipe

`build-qemu.sh` was run from an empty work directory (signed tarball,
patches 0001-0003) and `assemble.ps1` (now also runs on Windows PowerShell
5.1) assembled it with the approved swtpm and firmware:

| File | SHA-256 |
| --- | --- |
| QEMU executable (recipe, patches 0001-0003) | `0587429028595c427d9b95ac4f545a609076457a1cec33d25977329d16c8e387` |
| swtpm executable (unchanged) | `350e2698d00d3261ca1de550cbb51a75c44c8c0c903fea1cf7e1072a81318572` |
| Secure Boot code (unchanged) | `939bc01ba166b849f5296457b2658fff78fb622c8dc34cfa8204ba3ce2d88d2c` |
| Enrolled variables template (unchanged) | `a1003e1c5fa5e776b35392745f878f4a4bb38b611699e03ec3fce4a682e62a4f` |
| `bundle-recipe/SHA256SUMS` | `7528a78481ec095c954f9d48e5b48e1612c051a456a832301b8f4a74fdaddb5a` |

The three tests above passed together with it in 656 s (Setup reboots at
255 s and 379 s, then the guest's own reboot). Log
`E:\OpenVM-tests\win11\recipe-qual.log`; installed disk (reference for the
next tests) `E:\OpenVM-tests\lab\win11-real\installed-01a14729-4d12-7355-b12e-e2f7da0d20cf\`.
These are development hashes, not release approvals.

### Still open

After a shutdown or reset decided by the host (the test's ACPI shutdown,
or a QMP reset turned into a cold reboot), the next boot of these audit-mode
lab guests stopped at the logon screen with "L'account utente è stato
disattivato" (built-in Administrator disabled, no automatic audit logon),
with or without the installation ISO attached. Reboots requested by
Windows itself (Setup's and `shutdown /r`) kept the audit logon. This is
the lab guest's audit-mode behaviour, not a VM fault (Windows boots and
shows the logon screen); the lab scripts therefore only use reboots
requested by the guest. Not investigated further.

OOBE with a user account (done in §17),
BitLocker and Windows Hello, TPM-aware baseline/snapshot restore of an
installed system, the desktop UI's own console reconnection across a cold
reboot (the service ticket path is verified in §17), the release
bundle from the recipe (licences, signing), and whether the SEC-phase AP
fault of finding 3 also affects other WHPX profiles with more than one vCPU
(Vista Setup's first-reboot triple fault in R2c-01 has the same exit code).

## 16. BitLocker with a TPM protector across a cold reboot (2026-10-17)

`windows11_bitlocker_unlocks_from_the_tpm_after_a_cold_reboot` (ignored lab
test) runs the §15 installation, then, after the guest's own reboot:

1. BitLocker refuses while bootable media is attached (`0x80310030`, "rilevato
   supporto di avvio (CD o DVD)"), as on a physical PC. The lab script asks
   Windows to reboot; the test starts that boot without the installation ISO
   (the Tools ISO, not bootable, stays).
2. `Enable-BitLocker -TpmProtector -UsedSpaceOnly -SkipHardwareTest` on C:,
   then a reboot requested by Windows once encryption is under way: a cold
   reboot of this profile (new QEMU and swtpm, same TPM state).
3. Windows starts again from the partly encrypted volume, so the emulated TPM
   released the volume key at boot with no recovery prompt.

Passed in 760 s with the recipe bundle (§15):

```text
BitLocker before reboot:
VolumeStatus         : EncryptionInProgress
ProtectionStatus     : Off
LockStatus           : Unlocked
EncryptionPercentage : 71
KeyProtector         : {Tpm}
BitLocker reboot: cold reboot 5
BitLocker after reboot:
VolumeStatus         : EncryptionInProgress
ProtectionStatus     : Off
LockStatus           : Unlocked
EncryptionPercentage : 77
KeyProtector         : {Tpm}
```

`ProtectionStatus` stays `Off` until encryption completes; a fully encrypted
volume with protection on was not observed. Windows Hello, a TPM-aware
baseline/snapshot restore of an encrypted installed system, and an OOBE user
account remain open. Logs: `E:\OpenVM-tests\lab\win11-real\evidence-bitlocker-01a1477f-3df9-75b1-b064-7d84f927728a\`
(the disk was removed for space), test output
`E:\OpenVM-tests\win11\bitlocker-install3.log`.

## 17. OOBE with a local account, host reset, console across a cold reboot (2026-10-17)

- `windows11_completes_oobe_and_survives_a_host_reset` (ignored lab test,
  recipe bundle) installs with `win11-install-oobe.xml`: Setup checks as
  before, then OOBE with the documented unattend settings for a local lab
  account (`lightvm`, member of Administrators, automatic logon; password in
  clear in this disposable answer file only) instead of audit mode. Passed
  in 656 s: 2 Setup reboots, the reboot requested by the installed system,
  then a **Reset decided by the host** (QMP reset → cold reboot of the same
  run): Windows 11 Pro started again, logged on automatically, the Tools
  reconnected and the TBS probe reported `tpm_version=2`. The account runs
  without elevation after the automatic logon (UAC), so `Get-Tpm` is not
  used there. Unlike audit mode (§15, "Still open"), the host reset keeps
  the logon. Reference disk:
  `E:\OpenVM-tests\lab\win11-real\installed-01a148ab-32c0-7313-9ce1-e230788328e8`
  (the audit-mode reference disk of §15 was removed for space; logs in
  `evidence-01a14729`).
- `e2e_windows11_reset_is_a_cold_reboot_and_the_console_reconnects`
  (`tests/windows_real.rs`, ignored) goes through the whole service: ISO
  import (detected as Windows 11 x64), VM creation, start, the RFB console
  shows Windows Setup; then the **Reset action** from the IPC: the service
  log reads `guest reset: run … started again from cold`, the old console
  ends, the VM stays `running` with the same run, and a new console ticket
  for that run receives frames of the new process after about 1 s (the
  firmware's text after the reboot). Passed in 198 s. This exercises the
  ticket path the desktop uses to reconnect; the desktop UI itself was not
  driven. The lab test raises the host's free-storage figure so that the
  profile's 64 GiB sparse disk can be created on the lab volume.

## 18. Cold reboots for every WHPX run (2026-10-17)

Codex reported the same reset failure for installed Ubuntu 24.04 with the
ordinary upstream QEMU 11.1.0 (`failed to get xsave state`, then
`WHPX: Unexpected VP exit code 4` at the guest's warm reboot; R6-07b), and
Vista Setup's first reboot faults the same way (R2c-01). The cold reboot of
§15 therefore applies to **every run under WHPX**: the command line carries
`-action reboot=shutdown` whenever the accelerator is WHPX (KVM and HVF runs
reset in place), and the service keeps the launch inputs of every WHPX run
for `cold_reboot`.

Checks:

- `cmdline` unit test `whpx_runs_turn_resets_into_shutdowns` (all profiles
  under WHPX, none under KVM) and the exact-argv rows; workspace tests and
  clippy pass.
- `e2e_reset_is_a_cold_reboot_with_the_ordinary_qemu` (`windows_real.rs`,
  ignored): an Ubuntu VM booting the reference ISO with the ordinary QEMU,
  through the service; Reset → `guest reset: run … started again from cold`,
  the VM stays `running` in the same run, a new console ticket shows the new
  process after about 1 s. Passed in 136 s.

Not yet verified: a reboot requested by an installed Linux guest
(`installed_linux_guest_warm_reboot`, Codex's harness) and by Windows 10 or
older guests with this change.

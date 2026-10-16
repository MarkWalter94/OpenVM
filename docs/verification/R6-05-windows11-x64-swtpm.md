# R6-05 — Spike: Windows 11 x64 guest with emulated TPM 2.0 (swtpm) and Secure Boot on Windows hosts

- Execution date (UTC): 2026-10-15
- Executed by: development agent on the project host
- Roadmap step: section 12, R6; R2c
- Related ADRs: ADR-0035 (Windows 11 x64 with emulated TPM), ADR-0028 (swtpm and Secure Boot on macOS), ADR-0027 (Windows guest profiles), ADR-0010 (QEMU build), ADR-0011 (firmware)
- Status: PARTIAL — patched QEMU and swtpm verified with a Linux guest (TPM 2.0, persistence); Secure Boot OVMF not built on this host (antivirus), Windows 11 Setup not run (§12)

## Purpose

ADR-0035 leaves to a feasibility spike: where `swtpm` for Windows comes from (existing build or project build), the transport between the Windows build of QEMU and `swtpm`, the Secure Boot firmware for x86_64, and the proof of a Windows 11 x64 installation with TPM 2.0 and Secure Boot under WHPX. This file records what was executed on the project host, what was established only from sources and documentation, and what remains unverified.

What this file does NOT demonstrate: any product code (none is written); any host driver installation or elevated action (none is performed).

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

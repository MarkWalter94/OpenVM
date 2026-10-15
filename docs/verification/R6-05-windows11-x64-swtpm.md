# R6-05 — Spike: Windows 11 x64 guest with emulated TPM 2.0 (swtpm) and Secure Boot on Windows hosts

- Execution date (UTC): 2026-10-15
- Executed by: development agent on the project host
- Roadmap step: section 12, R6; R2c
- Related ADRs: ADR-0035 (Windows 11 x64 with emulated TPM), ADR-0028 (swtpm and Secure Boot on macOS), ADR-0027 (Windows guest profiles), ADR-0010 (QEMU build), ADR-0011 (firmware)
- Status: IN PROGRESS — findings are written incrementally while the spike runs

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


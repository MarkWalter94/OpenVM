# Project-built QEMU for the Windows 11 x64 profile (ADR-0035)

Upstream QEMU for Windows cannot have a TPM: `meson.build` refuses `CONFIG_TPM`
on Windows, and the `emulator` backend passes its data channel to `swtpm` as a
descriptor (`CMD_SET_DATAFD`, `SCM_RIGHTS`). This directory builds QEMU 11.1.0
for Windows x86_64 with two small patches so that `tpm-crb` works under WHPX
with a Windows build of `swtpm` connected over two loopback TCP sockets.
Every other profile keeps the upstream build of ADR-0010.

## Inputs

| Item | Value |
| --- | --- |
| Source | `https://download.qemu.org/qemu-11.1.0.tar.xz` |
| SHA-256 | `6ee1d1a61f68212476b27108c26da5f449dc09b626d42f8279ba0dc2e08fa858` |
| Signature | `qemu-11.1.0.tar.xz.sig`, key `CEAC C9E1 5534 EBAB B82D 3FA0 3353 C9CE F108 B584` (Michael Roth); `build-qemu.sh` checks `VALIDSIG` for this fingerprint |
| Toolchain | MSYS2 UCRT64 (base `msys2-base-x86_64-20260927.tar.xz`, SHA-256 `ea2f31a0b6ade63914ce441ffb022f0f6aa96982bfefa2326460a26d5fb01322`, signature by Christoph Reiter `0EBF 782C 5D53 F7E5 FB02 A667 46BD 761F 7A49 B0EC`); gcc 16.2.0, meson 1.12.1, ninja 1.13.2, glib2 2.90.1, gnutls 3.8.13, pixman 0.46.4; full package list with versions in `msys2-packages.txt` |
| Configure | `--target-list=x86_64-softmmu --enable-whpx --enable-tpm --enable-gnutls`, no docs/SDL/GTK/SPICE/OpenGL/curl/libssh/libnfs/capstone/guest agent/tools |

## Patches

- `0001-tpm-allow-the-emulator-backend-on-Windows-hosts.patch`
  - `meson.build`: `tpm` is no longer limited to POSIX hosts.
  - `qapi/tpm.json`: optional `data-chardev` in `TPMEmulatorOptions`.
  - `backends/tpm/tpm_emulator.c`: with `data-chardev=<id>` the TPM commands
    go over that chardev (for example a TCP socket chardev connected to
    `swtpm --server type=tcp`) instead of a socketpair passed with
    `CMD_SET_DATAFD`; `PTM_CAP_SET_DATAFD` is then not required. The TPM
    version is probed over the emulator's own transport (TPM2 `ReadClock`,
    then TPM 1.2 `GetTicks`) instead of `tpm_util_test_tpmdev()` on a raw
    descriptor. On Windows, omitting `data-chardev` is an error. On POSIX
    hosts the behaviour without `data-chardev` is unchanged.
- `0002-whpx-do-not-unmap-unaligned-sections.patch`
  - `accel/whpx/whpx-common.c`: a RAM section that is not page-aligned was
    turned into a `WHvUnmapGpaRange()` call, which WHPX rejects for an
    unaligned range, and QEMU aborted ("WHPX: failed to unmap GPA range").
    The `tpm-crb` command buffer (`0xFED40080`) and the TPM PPI RAM are such
    sections. They are never mapped, so the call is skipped; guest accesses
    exit to QEMU and are emulated.

## Use

```
swtpm socket --tpm2 --tpmstate dir=<vm>/tpm \
  --ctrl type=tcp,port=<P1>,bindaddr=127.0.0.1,terminate \
  --server type=tcp,port=<P2>,bindaddr=127.0.0.1

qemu-system-x86_64 ... \
  -chardev socket,id=chrtpm,host=127.0.0.1,port=<P1> \
  -chardev socket,id=chrtpmdata,host=127.0.0.1,port=<P2> \
  -tpmdev emulator,id=tpm0,chardev=chrtpm,data-chardev=chrtpmdata \
  -device tpm-crb,tpmdev=tpm0
```

`terminate` makes `swtpm` exit when QEMU closes the control connection.

## Build

In an MSYS2 UCRT64 shell, with the tarball and `.sig` downloaded into an
empty `<workdir>`:

```
JOBS=4 ./build-qemu.sh <workdir> <out>
```

The output contains the stripped `qemu-system-x86_64.exe`, its MSYS2 DLL
closure (25 DLLs: GLib, GnuTLS and its dependencies, pixman, zlib, zstd,
ncurses, brotli, …), `share/` with the pc-bios blobs of the same tarball and
the QEMU licence files, VNC keymaps and `SHA256SUMS`. The build is not bit-reproducible
(it embeds paths and timestamps): the released binaries are approved by
SHA-256 like the upstream build of ADR-0010.

Licences: QEMU GPL-2.0-only (with parts under other GPL-compatible licences);
the MSYS2 DLLs are LGPL-2.1+/GPL-3+ with exceptions/BSD/MIT. Shipping the
bundle needs the licence texts and the corresponding source (the QEMU tarball
plus these patches, and the MSYS2 source packages at the versions in
`msys2-packages.txt`).

Evidence: `docs/verification/R6-05-windows11-x64-swtpm.md`.

## Development product bundle (2026-10-16)

`assemble.ps1 -Qemu <qemu-output> -Swtpm <swtpm-output> -Firmware <firmware-output> -Out <new-directory>`
assembles `qemu/`, `swtpm/`, `firmware/` and an exact top-level manifest. It
prints the development approval line (`<hash> win11-bundle`), used by the
existing supervisor `--approved-hashes` option. The product verifies the
approved manifest, every listed file, no unlisted files or links, and holds
the files open against write/delete for the entire run.

The supervisor accepts `--win11-dir <bundle>`; otherwise it looks for `win11/`
below its ordinary QEMU directory. This bundle is selected only for the
`windows11_x64` guest. Other profiles keep their existing binary and firmware.
The desktop wizard recognizes Windows 11 x64 media and uses UEFI, 2 or more
vCPUs, at least 4096 MiB and 64 GiB. Missing/unapproved components refuse start.

Build tools, source trees, binaries and lab disks currently live on E: under
`E:\OpenVM-tests\win11`. The source/output build directories must be new or
empty; the build recipe does not recursively delete an existing directory.
The product bundle is experimental, not release-qualified. Its complete
runtime licence/source inventory and code signing remain release work.

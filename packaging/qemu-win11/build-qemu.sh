#!/bin/bash
# Builds QEMU 11.1.0 for Windows x86_64 (x86_64-softmmu, WHPX, TPM emulator
# backend) for the Windows 11 x64 profile (ADR-0035), from the signed
# upstream tarball plus the patches in this directory.
#
# Run in an MSYS2 UCRT64 shell (MSYSTEM=UCRT64) with the packages of
# msys2-packages.txt installed:
#   pacman -S --needed mingw-w64-ucrt-x86_64-{cc,meson,ninja,python,pkgconf,glib2,pixman,zlib,gnutls,libtasn1,zstd,python-tomli} \
#     make patch bison flex diffutils tar xz gnupg
#
# usage: build-qemu.sh <workdir> <out>
#   <workdir>/qemu-11.1.0.tar.xz and .sig must already be there (downloaded
#   into an empty directory from https://download.qemu.org/).
# Output in <out>: qemu-system-x86_64.exe (stripped), its MSYS2 DLL closure,
# share/ (pc-bios blobs of the same tarball), SHA256SUMS.
set -euo pipefail

QEMU_VERSION=11.1.0
QEMU_SHA256=6ee1d1a61f68212476b27108c26da5f449dc09b626d42f8279ba0dc2e08fa858
# Michael Roth, QEMU release signing key
QEMU_KEY=CEACC9E15534EBABB82D3FA03353C9CEF108B584
JOBS=${JOBS:-4}

HERE=$(cd "$(dirname "$0")" && pwd)
WORK=$(mkdir -p "$1" && cd "$1" && pwd)
OUT=$(mkdir -p "$2" && cd "$2" && pwd)
TARBALL=$WORK/qemu-$QEMU_VERSION.tar.xz

echo "$QEMU_SHA256 *$TARBALL" | sha256sum -c -
export GNUPGHOME=$WORK/gnupg
mkdir -p "$GNUPGHOME" && chmod 700 "$GNUPGHOME"
gpg --batch --quiet --list-keys "$QEMU_KEY" >/dev/null 2>&1 ||
  gpg --batch --keyserver hkps://keyserver.ubuntu.com --recv-keys "$QEMU_KEY"
gpg --batch --status-fd 1 --verify "$TARBALL.sig" "$TARBALL" 2>/dev/null |
  grep -q "VALIDSIG $QEMU_KEY " || { echo "bad QEMU signature" >&2; exit 1; }

SRC=$WORK/qemu-$QEMU_VERSION
rm -rf "$SRC"
# roms/ (firmware sources) holds symlinks MSYS2 cannot create and is not
# needed: the pc-bios blobs are used.
tar -xJf "$TARBALL" -C "$WORK" --exclude="qemu-$QEMU_VERSION/roms/*"
for p in "$HERE"/0*.patch; do
  patch -d "$SRC" -p1 --forward --no-backup-if-mismatch < "$p"
done

mkdir -p "$SRC/build"
cd "$SRC/build"
../configure --target-list=x86_64-softmmu --enable-whpx --enable-tpm --enable-gnutls \
  --disable-docs --disable-sdl --disable-gtk --disable-spice --disable-opengl \
  --disable-curl --disable-libssh --disable-libnfs --disable-capstone \
  --disable-guest-agent --disable-tools
grep -E "^ +(TPM support|WHPX support) +: YES" meson-logs/meson-log.txt >/dev/null ||
  { echo "TPM or WHPX not enabled" >&2; exit 1; }
# Only the emulator: the unit tests are not part of the distribution.
ninja -j"$JOBS" qemu-system-x86_64.exe

rm -rf "$OUT"/*
mkdir -p "$OUT/share"
cp qemu-system-x86_64.exe "$OUT/"
strip "$OUT/qemu-system-x86_64.exe"
ldd qemu-system-x86_64.exe | awk '$3 ~ /^\/ucrt64\// {print $3}' | sort -u |
  while read -r d; do cp "$d" "$OUT/"; done
for f in bios-256k.bin kvmvapic.bin linuxboot_dma.bin multiboot_dma.bin pvh.bin \
         vgabios-stdvga.bin vgabios-virtio.bin vgabios-qxl.bin vgabios-vmware.bin \
         vgabios-cirrus.bin vgabios-bochs-display.bin vgabios-ramfb.bin \
         efi-e1000e.rom efi-e1000.rom efi-virtio.rom efi-rtl8139.rom efi-ne2k_pci.rom efi-pcnet.rom; do
  cp "$SRC/pc-bios/$f" "$OUT/share/"
done
cp "$SRC/COPYING" "$SRC/COPYING.LIB" "$SRC/LICENSE" "$OUT/share/"
(cd "$OUT" && sha256sum -b *.exe *.dll share/* > SHA256SUMS && cat SHA256SUMS)

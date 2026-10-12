#!/bin/bash
# Builds the AArch64 UEFI firmware for Windows 11 ARM64 guests on macOS/HVF
# (ADR-0028): edk2-stable202408 ArmVirtQemu with Secure Boot and TPM2,
# network stack off, plus the local patch in this directory.
#
# Output in <out>:
#   lightvm-aarch64-code.fd       firmware code, padded to 64 MiB (pflash0)
#   lightvm-aarch64-vars-blank.fd empty variable store, 64 MiB
#   lightvm-aarch64-vars-win.fd   variable store with Secure Boot on: a
#                                 generated PK, Microsoft KEK 2011+2023 and
#                                 the Windows-only db (PCA 2011, UEFI CA 2023)
#   SHA256SUMS
#
# Requirements (macOS, Homebrew): git, python3, llvm, lld, acpica.
# usage: build.sh <workdir> <out>
set -euo pipefail

EDK2_TAG=edk2-stable202408
EDK2_COMMIT=b158dad150bf02879668f72ce306445250838201
VIRT_FIRMWARE=26.9
HERE=$(cd "$(dirname "$0")" && pwd)
WORK=$(mkdir -p "$1" && cd "$1" && pwd)
OUT=$(mkdir -p "$2" && cd "$2" && pwd)
LLVM=$(brew --prefix llvm)/bin

# Toolchain directory for CLANGDWARF: clang and binutils from Homebrew LLVM,
# ld.lld from Homebrew lld.
TC=$WORK/tc
mkdir -p "$TC"
for t in clang llvm-ar llvm-objcopy llvm-nm llvm-strip llvm-rc; do ln -sf "$LLVM/$t" "$TC/$t"; done
ln -sf "$(brew --prefix lld)/bin/ld.lld" "$TC/ld.lld"

# Sources at the pinned commit; submodules at the commits recorded by it.
SRC=$WORK/edk2
if [ ! -d "$SRC" ]; then
  git clone -q --depth 1 --branch "$EDK2_TAG" https://github.com/tianocore/edk2.git "$SRC"
fi
[ "$(git -C "$SRC" rev-parse HEAD)" = "$EDK2_COMMIT" ] || { echo "unexpected edk2 commit" >&2; exit 1; }
# Only the submodules the firmware uses (the unit test ones are not needed;
# subhook is no longer downloadable).
for m in ArmPkg/Library/ArmSoftFloatLib/berkeley-softfloat-3 \
         CryptoPkg/Library/OpensslLib/openssl \
         CryptoPkg/Library/MbedTlsLib/mbedtls \
         BaseTools/Source/C/BrotliCompress/brotli \
         MdeModulePkg/Library/BrotliCustomDecompressLib/brotli \
         MdePkg/Library/BaseFdtLib/libfdt \
         MdePkg/Library/MipiSysTLib/mipisyst \
         MdeModulePkg/Universal/RegularExpressionDxe/oniguruma \
         SecurityPkg/DeviceSecurity/SpdmLib/libspdm \
         RedfishPkg/Library/JsonLib/jansson; do
  git -C "$SRC" submodule update --init --depth 1 -q "$m"
done
git -C "$SRC" apply --check "$HERE/0001-BaseRngLib-AArch64-no-writable-global-in-XIP.patch" 2>/dev/null &&
  git -C "$SRC" apply "$HERE/0001-BaseRngLib-AArch64-no-writable-global-in-XIP.patch"

# Host tools. The 2024 BaseTools redefine UINT8_MAX, an error with current
# Apple clang; the tools run on the host only.
make -C "$SRC/BaseTools/Source/C" -j"$(sysctl -n hw.ncpu)" EXTRA_OPTFLAGS=-Wno-macro-redefined > "$WORK/basetools.log" 2>&1

(
  cd "$SRC"
  export CLANG_BIN=$TC/ PATH=$TC:$PATH PYTHON_COMMAND=python3
  # edksetup.sh reads the positional parameters and variables that may be
  # unset.
  set +u --
  # shellcheck disable=SC1091
  source edksetup.sh > /dev/null
  build -a AARCH64 -t CLANGDWARF -b RELEASE -p ArmVirtPkg/ArmVirtQemu.dsc -n "$(sysctl -n hw.ncpu)" \
    -D SECURE_BOOT_ENABLE=TRUE -D TPM2_ENABLE=TRUE -D NETWORK_ENABLE=FALSE > "$WORK/build.log" 2>&1
)
FV=$SRC/Build/ArmVirtQemu-AARCH64/RELEASE_CLANGDWARF/FV
cp "$FV/QEMU_EFI.fd" "$OUT/lightvm-aarch64-code.fd"
cp "$FV/QEMU_VARS.fd" "$OUT/lightvm-aarch64-vars-blank.fd"
truncate -s 64M "$OUT/lightvm-aarch64-code.fd" "$OUT/lightvm-aarch64-vars-blank.fd"

# Secure Boot keys with virt-firmware (pinned), in its own environment.
python3 -m venv "$WORK/venv"
"$WORK/venv/bin/pip" install -q "virt-firmware==$VIRT_FIRMWARE"
"$WORK/venv/bin/virt-fw-vars" --input "$OUT/lightvm-aarch64-vars-blank.fd" \
  --output "$OUT/lightvm-aarch64-vars-win.fd" \
  --enroll-generate "LightVM Platform Key" --microsoft-kek all --microsoft-db win11 --secure-boot > /dev/null

(cd "$OUT" && shasum -a 256 lightvm-aarch64-*.fd > SHA256SUMS && cat SHA256SUMS)

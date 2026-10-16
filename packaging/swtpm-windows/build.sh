#!/bin/bash
# Cygwin build for the Windows TPM helper, ADR-0035. No elevation or drivers.
# Inputs in WORK: {libtpms,swtpm}.tar.gz and .tar.gz.asc from the signed
# upstream v0.10.2 GitHub archives. Output must be a new/empty directory.
set -euo pipefail
WORK=$(cd "$1" && pwd)
mkdir -p "$2"
OUT=$(cd "$2" && pwd)
[[ -z $(find "$OUT" -mindepth 1 -maxdepth 1 -print -quit) ]] || { echo 'output must be empty' >&2; exit 1; }
KEY=B818B9CADF9089C2D5CEC66B75AD65802A0B4211
export GNUPGHOME="$WORK/gnupg"
mkdir -p "$GNUPGHOME"
chmod 700 "$GNUPGHOME"
gpg --batch --keyserver hkps://keyserver.ubuntu.com --recv-keys "$KEY"
echo "edac03680f8a4a1c5c1d609a10e3f41e1a129e38ff5158f0c8deaedc719fb127 *$WORK/libtpms.tar.gz" | sha256sum -c -
echo "f61cf6f1e9bbcb4cefb30b70cafaf1c4df54c6961e65cfa63830e8ad0e220134 *$WORK/swtpm.tar.gz" | sha256sum -c -
for name in libtpms swtpm; do
    status=$(gpg --batch --status-fd 1 --verify "$WORK/$name.tar.gz.asc" "$WORK/$name.tar.gz" 2>/dev/null)
    grep -Eq "^\[GNUPG:\] VALIDSIG .*($KEY)( |$)" <<< "$status" || { echo "signature refused: $name" >&2; exit 1; }
    [[ -d "$WORK/$name-0.10.2" ]] || tar -xzf "$WORK/$name.tar.gz" -C "$WORK"
done
PREFIX="$WORK/prefix"
cd "$WORK/libtpms-0.10.2"
./autogen.sh --prefix="$PREFIX" --with-openssl --with-tpm2 --disable-static
make -j"${JOBS:-4}"
make install
cd "$WORK/swtpm-0.10.2"
PKG_CONFIG_PATH="$PREFIX/lib/pkgconfig" ./autogen.sh --prefix="$PREFIX" --with-gnutls --without-seccomp --disable-tests
make -j"${JOBS:-4}"
make install
export PATH="$PREFIX/bin:$PATH"
cp "$PREFIX/bin/swtpm.exe" "$OUT/"
# cygcheck reports the full transitive runtime closure. Copy Cygwin/prefix
# DLLs only; system DLLs stay provided by Windows.
cygcheck "$PREFIX/bin/swtpm.exe" > "$WORK/runtime-closure.txt"
while IFS= read -r line; do
    line=$(echo "$line" | sed 's/^[[:space:]]*//;s/\r$//')
    [[ "$line" == *.dll ]] || continue
    file=$(cygpath -u "$line")
    case "$file" in /usr/bin/*|/bin/*|"$PREFIX"/bin/*) cp "$file" "$OUT/" ;; esac
done < "$WORK/runtime-closure.txt"
cp "$WORK/libtpms-0.10.2/LICENSE" "$OUT/LICENSE-libtpms"
cp "$WORK/swtpm-0.10.2/LICENSE" "$OUT/LICENSE-swtpm"
(cd "$OUT" && sha256sum -b *.exe *.dll LICENSE-* > SHA256SUMS)
"$OUT/swtpm.exe" socket --print-capabilities
echo 'Build complete. Distribution additionally requires the runtime licence/source inventory.'

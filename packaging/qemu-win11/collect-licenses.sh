#!/bin/bash
# Licence texts and source inventory of the MSYS2 DLL closure of a QEMU
# built by build-qemu.sh (ADR-0035, R6-05). Run in the same MSYS2 UCRT64
# installation that built it, so that pacman knows the installed packages.
#
# usage: collect-licenses.sh <qemu-out> <dest>
# <dest> must not exist. Output:
#   <dest>/<package>/...      licence files from /ucrt64/share/licenses
#   <dest>/INVENTORY.tsv      file, package, version, licences, source
#   <dest>/MISSING.txt        packages without a licence file (must be empty
#                             before a distribution)
# QEMU itself is covered by share/COPYING, COPYING.LIB and LICENSE of the
# build output; its corresponding source is the signed tarball plus the
# patches of this directory.
set -euo pipefail
export LANG=C LC_ALL=C

OUT=$(cd "$1" && pwd)
[[ ! -e "$2" ]] || { echo 'destination must not exist' >&2; exit 1; }
mkdir -p "$2"
DEST=$(cd "$2" && pwd)
PREFIX=mingw-w64-ucrt-x86_64-

printf 'file\tpackage\tversion\tlicences\tsource\n' >"$DEST/INVENTORY.tsv"
: >"$DEST/MISSING.txt"
declare -A seen=()
for dll in "$OUT"/*.dll; do
  name=$(basename "$dll")
  pkg=$(pacman -Qqo "/ucrt64/bin/$name" 2>/dev/null) ||
    { echo "$name: not owned by an installed package" >>"$DEST/MISSING.txt"; continue; }
  info=$(pacman -Qi "$pkg")
  version=$(sed -n 's/^Version *: //p' <<<"$info")
  licences=$(sed -n 's/^Licenses *: //p' <<<"$info" | tr -s ' ')
  short=${pkg#"$PREFIX"}
  # MSYS2 publishes the source of every package build next to the binaries,
  # named after the package base (for example mingw-w64-gettext).
  base=$(sed -n '/^%BASE%$/{n;p;}' /var/lib/pacman/local/"$pkg-$version"/desc)
  source="https://repo.msys2.org/mingw/sources/${base}-${version}.src.tar.zst"
  printf '%s\t%s\t%s\t%s\t%s\n' "$name" "$pkg" "$version" "$licences" "$source" >>"$DEST/INVENTORY.tsv"
  [[ -z ${seen[$pkg]:-} ]] || continue
  seen[$pkg]=1
  if [[ -d /ucrt64/share/licenses/$short ]]; then
    cp -R "/ucrt64/share/licenses/$short" "$DEST/$short"
  else
    echo "$pkg $version ($licences): no /ucrt64/share/licenses/$short" >>"$DEST/MISSING.txt"
  fi
done
# FETCH=1: the corresponding MSYS2 source packages, signature-checked with
# the pacman keyring, into <dest>/sources; for packages without an installed
# licence file, the COPYING/LICENSE files of the upstream archive inside.
if [[ ${FETCH:-0} == 1 ]]; then
  mkdir -p "$DEST/sources"
  : >"$DEST/MISSING.fetched.txt"
  tail -n +2 "$DEST/INVENTORY.tsv" | cut -f2,5 | sort -u | while IFS=$'\t' read -r pkg url; do
    file="$DEST/sources/$(basename "$url")"
    curl --retry 3 -fsSLo "$file" "$url"
    curl --retry 3 -fsSLo "$file.sig" "$url.sig"
    # A private copy of the pacman keyring (MSYS2 packager keys only):
    # pacman-key itself needs administrator rights.
    if [[ -z ${KEYRING:-} ]]; then
      KEYRING=$(mktemp -d)
      find /etc/pacman.d/gnupg -maxdepth 1 -type f -exec cp {} "$KEYRING/" \;
    fi
    # A good signature needs GOODSIG and VALIDSIG; VALIDSIG alone is also
    # emitted for signatures by revoked or expired keys (REVKEYSIG,
    # EXPKEYSIG) and for expired signatures (EXPSIG).
    status=$(gpg --homedir "$KEYRING" --batch --no-auto-check-trustdb --status-fd 1 \
      --verify "$file.sig" "$file" 2>/dev/null) || status=
    grep -q '^\[GNUPG:\] GOODSIG ' <<<"$status" &&
      grep -q '^\[GNUPG:\] VALIDSIG ' <<<"$status" &&
      ! grep -qE '^\[GNUPG:\] (REVKEYSIG|EXPKEYSIG|EXPSIG|BADSIG|ERRSIG) ' <<<"$status" ||
      { echo "$pkg: bad or unknown signature on $(basename "$url")" >&2; exit 1; }
    short=${pkg#"$PREFIX"}
    [[ ! -d "$DEST/$short" ]] || continue
    work=$(mktemp -d)
    tar -xf "$file" -C "$work"
    found=0
    for inner in "$work"/*/*.tar.* "$work"/*/*.tgz; do
      [[ -f $inner ]] || continue
      mkdir -p "$work/up"
      tar -xf "$inner" -C "$work/up" --wildcards --no-anchored \
        'COPYING*' 'LICENSE*' 'LICENCE*' 2>/dev/null || true
    done
    while IFS= read -r -d '' lic; do
      rel=${lic#"$work/up/"}
      # Top-level licence files of the upstream tree only.
      [[ $(tr -cd '/' <<<"$rel" | wc -c) -le 1 ]] || continue
      mkdir -p "$DEST/$short"
      cp "$lic" "$DEST/$short/$(basename "$lic")"
      found=1
    done < <(find "$work/up" -type f -print0 2>/dev/null)
    rm -rf "$work"
    [[ $found == 1 ]] || echo "$pkg: no licence file in the source either" >>"$DEST/MISSING.fetched.txt"
  done
  echo "still missing after fetching the sources:"
  cat "$DEST/MISSING.fetched.txt"
  # MISSING.txt is what remains unresolved.
  mv "$DEST/MISSING.txt" "$DEST/MISSING.installed.txt"
  mv "$DEST/MISSING.fetched.txt" "$DEST/MISSING.txt"
fi
cat "$DEST/INVENTORY.tsv"
echo "missing (installed licence files):"
cat "$DEST/MISSING.installed.txt" 2>/dev/null || cat "$DEST/MISSING.txt"

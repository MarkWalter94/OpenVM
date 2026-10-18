#!/bin/bash
# Licence texts and corresponding sources of the Cygwin DLL closure of the
# swtpm built by build.sh (ADR-0035, R6-05). Run in the same Cygwin
# installation, with the setup.ini that installed it.
#
# usage: collect-licenses.sh <swtpm-out> <setup.ini> <dest>
# <setup.ini>.sig (the detached signature published beside setup.ini on the
# mirror) must be next to <setup.ini>: it is verified with gpg against the
# Cygwin signing key 56405CF6FCC81574682A5D561A698DE9E2E56300, taken from
# CYGWIN_KEY (a file, e.g. https://cygwin.com/key/pubring.asc) or, if
# unset, fetched by fingerprint from keyserver.ubuntu.com, in a private
# keyring. Without a good signature by that key nothing is collected.
# <dest> must not exist. Output:
#   <dest>/<package>/...     licence files (installed docs, else from source)
#   <dest>/sources/          Cygwin source packages, SHA-512 checked against
#                            the signature-checked setup.ini
#   <dest>/INVENTORY.tsv     file, package, version, source
#   <dest>/MISSING.txt       entries to resolve before a distribution
# libtpms (cygtpms-0.dll) and swtpm are built from source by build.sh and
# covered by LICENSE-libtpms and LICENSE-swtpm of its output.
set -euo pipefail
export LANG=C LC_ALL=C

OUT=$(cd "$1" && pwd)
INI=$2
[[ ! -e "$3" ]] || { echo 'destination must not exist' >&2; exit 1; }

# setup.ini is the hash source of the source packages: it must carry a good
# signature (GOODSIG and VALIDSIG, no revoked/expired key or signature) by
# the Cygwin key, whose fingerprint is recorded in
# docs/verification/R6-05-windows11-x64-swtpm.md.
CYGWIN_FPR=56405CF6FCC81574682A5D561A698DE9E2E56300
[[ -f $INI && -f $INI.sig ]] ||
  { echo "$INI and its detached signature $INI.sig are required" >&2; exit 1; }
GPGDIR=$(mktemp -d)
trap 'rm -rf "$GPGDIR"' EXIT
chmod 700 "$GPGDIR"
if [[ -n ${CYGWIN_KEY:-} ]]; then
  gpg --homedir "$GPGDIR" --batch --import "$CYGWIN_KEY" 2>/dev/null
else
  gpg --homedir "$GPGDIR" --batch --keyserver hkps://keyserver.ubuntu.com \
    --recv-keys "$CYGWIN_FPR" 2>/dev/null
fi
status=$(gpg --homedir "$GPGDIR" --batch --status-fd 1 \
  --verify "$INI.sig" "$INI" 2>/dev/null) || status=
grep -q '^\[GNUPG:\] GOODSIG ' <<<"$status" &&
  ! grep -qE '^\[GNUPG:\] (REVKEYSIG|EXPKEYSIG|EXPSIG|BADSIG|ERRSIG) ' <<<"$status" &&
  awk -v fpr="$CYGWIN_FPR" '$1 == "[GNUPG:]" && $2 == "VALIDSIG" &&
    (toupper($3) == fpr || toupper($NF) == fpr) { ok = 1 } END { exit !ok }' <<<"$status" ||
  { echo "$INI: no good signature by the Cygwin key $CYGWIN_FPR" >&2; exit 1; }

mkdir -p "$3/sources"
DEST=$(cd "$3" && pwd)
MIRROR=${MIRROR:-https://mirrors.kernel.org/sourceware/cygwin}

# "source: <path> <size> <sha512>" of package $1 at version $2 in setup.ini.
source_of() {
  awk -v name="@ $1" -v ver="version: $2" '
    $0 == name { inpkg = 1; next }
    /^@ / { inpkg = 0 }
    inpkg && $0 == ver { inver = 1; next }
    inpkg && /^version: / { inver = 0 }
    inpkg && inver && /^source: / { print $2, $3, $4; exit }
  ' "$INI"
}

printf 'file\tpackage\tversion\tsource\n' >"$DEST/INVENTORY.tsv"
: >"$DEST/MISSING.txt"
declare -A seen=()
for dll in "$OUT"/*.dll; do
  name=$(basename "$dll")
  owner=$(cygcheck -f "/usr/bin/$name" | head -n 1)
  if [[ -z $owner ]]; then
    printf '%s\t(built by build.sh)\t-\t-\n' "$name" >>"$DEST/INVENTORY.tsv"
    continue
  fi
  pkg=$(awk -v f="$owner" 'index($2, f".") == 1 { print $1; exit }' /etc/setup/installed.db)
  version=${owner#"$pkg"-}
  read -r path size sha <<<"$(source_of "$pkg" "$version")" || true
  if [[ -z ${path:-} ]]; then
    echo "$name: no source entry for $pkg $version in setup.ini" >>"$DEST/MISSING.txt"
    continue
  fi
  printf '%s\t%s\t%s\t%s\n' "$name" "$pkg" "$version" "$MIRROR/$path" >>"$DEST/INVENTORY.tsv"
  [[ -z ${seen[$path]:-} ]] || continue
  seen[$path]=1
  file="$DEST/sources/$(basename "$path")"
  # The curl found first may be a Windows build (Git for Windows).
  curl --retry 3 -fsSLo "$(cygpath -w "$file")" "$MIRROR/$path"
  [[ $(sha512sum "$file" | cut -d' ' -f1) == "$sha" ]] ||
    { echo "$path: SHA-512 differs from setup.ini" >&2; exit 1; }
  # Licence texts: the package's installed docs, else the upstream archive.
  mkdir -p "$DEST/$pkg"
  while read -r doc; do
    cp "/$doc" "$DEST/$pkg/"
  done < <(zcat "/etc/setup/$pkg.lst.gz" | grep -E '^usr/share/doc/[^/]+/(COPYING|LICEN[CS]E|CYGWIN_LICENSE)[^/]*$' || true)
  if [[ -z $(ls -A "$DEST/$pkg") ]]; then
    work=$(mktemp -d)
    tar -xf "$file" -C "$work"
    for inner in "$work"/*/*.tar.* "$work"/*/*.tgz; do
      [[ -f $inner ]] || continue
      mkdir -p "$work/up"
      tar -xf "$inner" -C "$work/up" --wildcards --no-anchored \
        'COPYING*' 'LICENSE*' 'LICENCE*' '*/LICENSES/*' 2>/dev/null ||
        # .tar.lz (gettext) without lzip: Windows' bsdtar reads it.
        /cygdrive/c/Windows/System32/tar.exe -xf "$(cygpath -w "$inner")" \
          -C "$(cygpath -w "$work/up")" '*/COPYING*' '*/LICENSE*' '*/LICENCE*' \
          '*/intl/COPYING*' 2>/dev/null || true
    done
    while IFS= read -r -d '' lic; do
      rel=${lic#"$work/up/"}
      # Top-level licence files, a top-level LICENSES/ directory (glib), and
      # the licence of gettext's libintl (gettext-runtime/intl/COPYING.LIB).
      [[ $(tr -cd '/' <<<"$rel" | wc -c) -le 1 || $rel == */LICENSES/* ||
         $rel == */gettext-runtime/intl/COPYING* ]] || continue
      [[ $rel != */LICENSES/*/* ]] || continue
      cp "$lic" "$DEST/$pkg/$(basename "$lic")"
    done < <(find "$work/up" -type f -print0 2>/dev/null)
    rm -rf "$work"
  fi
  [[ -n $(ls -A "$DEST/$pkg") ]] || echo "$pkg $version: no licence file found" >>"$DEST/MISSING.txt"
done
cat "$DEST/INVENTORY.tsv"
echo "missing:"
cat "$DEST/MISSING.txt"

#!/bin/sh
set -eu
script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
script="$script_dir/TOOLS.PY"
# Linux ISO9660 mounts normally expose the uppercase ISO names in lowercase.
if [ ! -f "$script" ]; then script="$script_dir/tools.py"; fi
exec python3 "$script" --install

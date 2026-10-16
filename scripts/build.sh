#!/bin/bash
# Builds LightVM on macOS or Linux (counterpart of scripts/build.ps1).
#
#   default        cargo build --workspace --locked
#   --release      cargo build --release --locked of the shipped binaries
#                  (lightvm-desktop, lightvm-service, lightvm-mcp)
#   --dev-tools    adds the lightvm-desktop `dev-tools` feature (--smoke,
#                  --static, --simulated-console, --no-supervisor)
#   --run          starts lightvm-desktop after a successful build; the
#                  arguments after `--` are passed to it
#   --target-dir D CARGO_TARGET_DIR for the build
#
# Exit code: the cargo (or lightvm-desktop) exit code.
#
# Examples:
#   scripts/build.sh
#   scripts/build.sh --release --run
#   scripts/build.sh --dev-tools --run -- --simulated-console
#
# macOS: the product cannot start VMs yet (macOS host layer missing,
# ADR-0028); the app, the catalog and the media registry work.
set -euo pipefail

REPO=$(cd "$(dirname "$0")/.." && pwd)
release=0
devtools=0
run=0
run_args=()
while [ $# -gt 0 ]; do
  case "$1" in
    --release) release=1 ;;
    --dev-tools) devtools=1 ;;
    --run) run=1 ;;
    --target-dir) export CARGO_TARGET_DIR="$2"; shift ;;
    --) shift; run_args=("$@"); break ;;
    -h|--help) sed -n '2,22p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) echo "unknown option $1 (see --help)" >&2; exit 2 ;;
  esac
  shift
done

# rustup from Homebrew is keg-only: put it first when present.
for d in /opt/homebrew/opt/rustup/bin /usr/local/opt/rustup/bin "$HOME/.cargo/bin"; do
  [ -d "$d" ] && PATH="$d:$PATH"
done
export PATH
command -v cargo > /dev/null || { echo "cargo not found: install rustup (brew install rustup)" >&2; exit 1; }

args=(build --locked)
if [ $release -eq 1 ]; then
  args+=(--release -p lightvm-desktop -p lightvm-service -p lightvm-mcp)
else
  args+=(--workspace)
fi
[ $devtools -eq 1 ] && args+=(--features lightvm-desktop/dev-tools)

echo "==> cargo ${args[*]}"
cd "$REPO"
code=0
cargo "${args[@]}" || code=$?
if [ $code -ne 0 ]; then
  echo "==> build FAILED (exit $code)"
  exit $code
fi

out="${CARGO_TARGET_DIR:-$REPO/target}/$([ $release -eq 1 ] && echo release || echo debug)"
echo "==> build OK: $out"

if [ $run -eq 1 ]; then
  echo "==> $out/lightvm-desktop ${run_args[*]-}"
  exec "$out/lightvm-desktop" ${run_args[@]+"${run_args[@]}"}
fi

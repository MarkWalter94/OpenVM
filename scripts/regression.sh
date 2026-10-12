#!/bin/bash
# Runs the blocking LightVM suites on macOS or Linux and summarises the
# results (counterpart of scripts/regression.ps1).
#
# Steps (each with its own log in the output directory):
#   fmt              cargo fmt --all --check
#   clippy           cargo clippy --workspace --all-targets --locked -D warnings
#   clippy-ui-glow   lightvm-desktop with the glow backend only
#   clippy-ui-wgpu   lightvm-desktop with the wgpu backend only
#   clippy-client    lightvm-service --no-default-features
#   test             cargo test --workspace --locked --no-fail-fast
#   schema           lightvm-proto schema snapshot
#   licences         cargo deny --locked check when cargo-deny is installed;
#                    otherwise the offline lightvm-sbom check of the declared
#                    licences for every target of deny.toml
#   sbom             CycloneDX SBOM of the shipped binaries
#   release-build    (--release) release build of the shipped binaries
#
# The real-QEMU and real-client tests (#[ignore]) are not run here
# (docs/TESTING.md).
#
# Options:
#   --out-dir D       logs and summary (default target/regression/<UTC stamp>)
#   --target-dir D    CARGO_TARGET_DIR (default target/regression-build)
#   --steps a,b       subset of the steps above (default: all but release-build)
#   --release         adds release-build
#   --no-debuginfo    dev/test profile without debug info (less disk space)
#
# Exit code: 0 if every executed step passed, 1 otherwise.
set -uo pipefail

REPO=$(cd "$(dirname "$0")/.." && pwd)
stamp=$(date -u +%Y%m%dT%H%M%SZ)
out_dir="$REPO/target/regression/$stamp"
target_dir="$REPO/target/regression-build"
steps=""
release=0
while [ $# -gt 0 ]; do
  case "$1" in
    --out-dir) out_dir="$2"; shift ;;
    --target-dir) target_dir="$2"; shift ;;
    --steps) steps="$2"; shift ;;
    --release) release=1 ;;
    --no-debuginfo) export CARGO_PROFILE_DEV_DEBUG=0 ;;
    -h|--help) sed -n '2,32p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) echo "unknown option $1 (see --help)" >&2; exit 2 ;;
  esac
  shift
done
mkdir -p "$out_dir"
out_dir=$(cd "$out_dir" && pwd)

for d in /opt/homebrew/opt/rustup/bin /usr/local/opt/rustup/bin "$HOME/.cargo/bin"; do
  [ -d "$d" ] && PATH="$d:$PATH"
done
export PATH CARGO_TARGET_DIR="$target_dir" CARGO_INCREMENTAL=0
command -v cargo > /dev/null || { echo "cargo not found: install rustup (brew install rustup)" >&2; exit 1; }
cd "$REPO"

have_deny=0
cargo deny --version > /dev/null 2>&1 && have_deny=1

all_steps="fmt clippy clippy-ui-glow clippy-ui-wgpu clippy-client test schema licences sbom release-build"

run_step() {
  case "$1" in
    fmt) cargo fmt --all --check ;;
    clippy) cargo clippy --workspace --all-targets --locked -- -D warnings ;;
    clippy-ui-glow) cargo clippy -p lightvm-desktop --no-default-features --features glow --all-targets --locked -- -D warnings ;;
    clippy-ui-wgpu) cargo clippy -p lightvm-desktop --no-default-features --features wgpu --all-targets --locked -- -D warnings ;;
    clippy-client) cargo clippy -p lightvm-service --no-default-features --all-targets --locked -- -D warnings ;;
    test) cargo test --workspace --locked --no-fail-fast ;;
    schema) cargo test --locked -p lightvm-proto --test schema_snapshot ;;
    licences)
      if [ $have_deny -eq 1 ]; then
        cargo deny --locked check
      else
        local t
        for t in x86_64-pc-windows-msvc x86_64-unknown-linux-gnu aarch64-apple-darwin; do
          cargo run --locked --offline -q -p lightvm-sbom -- --workspace . --target "$t" \
            --out "$out_dir/licences-$t.cdx.json" --check-licences deny.toml || return $?
        done
      fi ;;
    sbom) cargo run --locked --offline -q -p lightvm-sbom -- --workspace . \
            --out "$out_dir/lightvm.cdx.json" --notices "$out_dir/THIRD-PARTY-NOTICES.txt" ;;
    release-build) cargo build --release --locked -p lightvm-desktop -p lightvm-service -p lightvm-mcp \
            --bin lightvm-desktop --bin lightvm-service --bin lightvm-mcp ;;
    *) return 99 ;;
  esac
}

if [ -z "$steps" ]; then
  steps="fmt clippy clippy-ui-glow clippy-ui-wgpu clippy-client test schema licences sbom"
else
  steps=$(echo "$steps" | tr ',' ' ')
fi
if [ $release -eq 1 ] && ! echo " $steps " | grep -q " release-build "; then
  steps="$steps release-build"
fi
for s in $steps; do
  echo " $all_steps " | grep -q " $s " || { echo "unknown step $s (known: $all_steps)" >&2; exit 2; }
done

summary="$out_dir/summary.txt"
dirty=""
[ -n "$(git status --porcelain --untracked-files=no)" ] && dirty=" (uncommitted changes)"
{
  echo "LightVM regression $stamp ($(uname -sm))"
  echo "Commit: $(git rev-parse HEAD)$dirty"
  echo "Target dir: $target_dir"
  echo
  printf '%-16s %-10s %7s  %s\n' Step Result Seconds Detail
} > "$summary"

failed=0
for s in $steps; do
  log="$out_dir/$s.log"
  echo "==> $s"
  t0=$(date +%s)
  run_step "$s" > "$log" 2>&1
  code=$?
  secs=$(( $(date +%s) - t0 ))
  detail=""
  case "$s" in
    test|schema)
      detail=$(awk '/^test result:/ {p+=$4; f+=$6; i+=$8} END {printf "%d passed, %d failed, %d ignored", p, f, i}' "$log")
      fails=$(grep -E '^test .* \.\.\. FAILED' "$log" | awk '{print $2}' | paste -sd, -)
      [ -n "$fails" ] && detail="$detail; failed: $fails" ;;
    clippy*) detail="$(grep -cE '^(warning|error)(\[[^]]*\])?: ' "$log") diagnostics" ;;
    licences) [ $have_deny -eq 1 ] && detail="cargo-deny" || detail="offline lightvm-sbom check (cargo-deny not installed)" ;;
  esac
  if [ $code -eq 0 ]; then result=PASS; else result="FAIL ($code)"; failed=1; fi
  echo "    $result in $secs s $detail"
  printf '%-16s %-10s %7s  %s\n' "$s" "$result" "$secs" "$detail" >> "$summary"
done

echo
cat "$summary"
echo "Logs: $out_dir"
exit $failed

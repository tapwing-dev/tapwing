#!/usr/bin/env bash
# iOS hierarchy parity: navigate with Tapwing, then dump `maestro hierarchy`
# and `tapwing hierarchy` of the same screen and diff them.
#   bench/ios/capture_hierarchies.sh <simulator-udid> [out-dir]
set -euo pipefail
cd "$(dirname "$0")/../.."
UDID="${1:?simulator UDID}"
OUT="${2:-bench/results/ios-simulator/hierarchy}"
TAPWING="${TAPWING_BIN:-./crates/tapwing/target/release/tapwing}"
mkdir -p "$OUT"
: > "$OUT/summary.txt"
for flow in bench/ios/hierarchy/*.yaml; do
  name=$(basename "$flow" .yaml)
  "$TAPWING" test "$flow" --device "$UDID" >"$OUT/$name-nav.log" 2>&1 || { echo "$name: navigation FAILED" | tee -a "$OUT/summary.txt"; continue; }
  maestro --device "$UDID" hierarchy >"$OUT/$name-maestro.json" 2>"$OUT/$name-maestro.err"
  "$TAPWING" hierarchy --device "$UDID" --app-id dev.tapwing.sampleApp >"$OUT/$name-tapwing.json"
  printf '%s: ' "$name" | tee -a "$OUT/summary.txt"
  python3 -I bench/tools/diff_hierarchy_ios.py "$OUT/$name-maestro.json" "$OUT/$name-tapwing.json" >"$OUT/$name-diff.txt" || true
  tail -1 "$OUT/$name-diff.txt" | tee -a "$OUT/summary.txt"
done

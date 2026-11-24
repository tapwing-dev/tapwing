#!/usr/bin/env bash
# iOS simulator differential run: every flow in examples/sample_app/.maestro/ios
# in Tapwing (driver build, --check-settle) and in Maestro 2.12.1 (plain build).
#   TAPWING_APP=<Runner.app with lib/main_tapwing.dart> PLAIN_APP=<Runner.app with lib/main.dart> \
#     bench/ios/run_ios_differential.sh <simulator-udid> [out-dir] [flow...]
set -uo pipefail
cd "$(dirname "$0")/../.."
UDID="${1:?simulator UDID}"; shift
OUT="${1:-bench/results/ios-simulator/differential}"; shift || true
TAPWING="${TAPWING_BIN:-./crates/tapwing/target/release/tapwing}"
: "${TAPWING_APP:?path to the driver build (.app)}" "${PLAIN_APP:?path to the plain build (.app)}"
FLOWS=("$@")
[ ${#FLOWS[@]} -eq 0 ] && FLOWS=(examples/sample_app/.maestro/ios/*.yaml examples/sample_app/.maestro/ios/negative/*.yaml)
mkdir -p "$OUT"
now() { python3 -c 'import time; print(time.time())'; }
APPEARANCE=$(xcrun simctl ui "$UDID" appearance)
for flow in "${FLOWS[@]}"; do
  name=$(basename "$flow" .yaml); case "$flow" in */negative/*) name="neg-$name";; esac
  xcrun simctl install "$UDID" "$TAPWING_APP"
  s=$(now)
  "$TAPWING" test "$flow" --device "$UDID" --check-settle --report "$OUT/tapwing-$name.json" \
    --test-output-dir "$OUT/tapwing-$name-artifacts" >"$OUT/tapwing-$name.log" 2>&1; krc=$?
  e=$(now); kw=$(python3 -c "print('%.2f' % ($e - $s))")
  xcrun simctl install "$UDID" "$PLAIN_APP"
  s=$(now)
  maestro --device "$UDID" test --test-output-dir "$OUT/maestro-$name" "$flow" >"$OUT/maestro-$name.log" 2>&1; mrc=$?
  e=$(now); mw=$(python3 -c "print('%.2f' % ($e - $s))")
  echo "$name tapwing rc=$krc ${kw}s maestro rc=$mrc ${mw}s" | tee -a "$OUT/wall.txt"
done
xcrun simctl ui "$UDID" appearance "$APPEARANCE"

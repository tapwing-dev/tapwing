#!/usr/bin/env bash
# iOS simulator benchmark: the same flow on the same simulator, Maestro vs Tapwing.
#   TAPWING_APP=<driver Runner.app> PLAIN_APP=<plain Runner.app> \
#     bench/ios/run_ios_bench.sh [runs] <simulator-udid>
#   OUT=... to change the results dir (default bench/results/ios-simulator/bench)
set -euo pipefail
cd "$(dirname "$0")/../.."
RUNS="${1:-3}"
UDID="${2:?simulator UDID}"
FLOW="${FLOW:-examples/sample_app/.maestro/ios/login_and_browse.yaml}"
OUT="${OUT:-bench/results/ios-simulator/bench}"
TAPWING="${TAPWING_BIN:-./crates/tapwing/target/release/tapwing}"
: "${TAPWING_APP:?path to the driver build (.app)}" "${PLAIN_APP:?path to the plain build (.app)}"
rm -rf "$OUT" && mkdir -p "$OUT"
now() { python3 -c 'import time; print(time.time())'; }

echo "== Maestro $(maestro --version 2>/dev/null | tail -1) on the plain build (no driver)"
xcrun simctl install "$UDID" "$PLAIN_APP"
for i in $(seq 1 "$RUNS"); do
  s=$(now)
  maestro --device "$UDID" test --debug-output "$OUT/maestro-$i" "$FLOW" >"$OUT/maestro-$i.log" 2>&1 || echo "maestro run $i FAILED"
  e=$(now)
  python3 -c "print('maestro run $i: %.1f s' % ($e - $s))" | tee -a "$OUT/wall.txt"
done

echo "== Tapwing on the driver build"
xcrun simctl install "$UDID" "$TAPWING_APP"
for i in $(seq 1 "$RUNS"); do
  s=$(now)
  "$TAPWING" test "$FLOW" --device "$UDID" --check-settle --report "$OUT/tapwing-$i.json" >"$OUT/tapwing-$i.log" 2>&1 || echo "tapwing run $i FAILED"
  e=$(now)
  python3 -c "print('tapwing run $i: %.1f s' % ($e - $s))" | tee -a "$OUT/wall.txt"
done

python3 -I bench/tools/compare.py "$OUT" | tee "$OUT/summary.md"

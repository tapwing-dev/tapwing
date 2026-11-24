#!/usr/bin/env bash
# F0 benchmark: same Maestro flow on the same device, Maestro vs Tapwing.
#   bench/run_bench.sh [runs] [device]
#   TAPWING_ONLY=1 bench/run_bench.sh   # re-run Tapwing, keep Maestro results
#   OUT=bench/results/pixel-8a bench/run_bench.sh 3 <serial>   # results dir (default bench/results/<serial>)
set -euo pipefail
cd "$(dirname "$0")/.."
RUNS="${1:-3}"
DEVICE="${2:-emulator-5554}"
FLOW=examples/sample_app/.maestro/login_and_browse.yaml
OUT="${OUT:-bench/results/$DEVICE}"   # e.g. OUT=bench/results/pixel-8a
TAPWING="${TAPWING_BIN:-./crates/tapwing/target/release/tapwing}"
if [ "${TAPWING_ONLY:-0}" = "1" ]; then
  rm -f "$OUT"/tapwing-* && grep -v '^tapwing' "$OUT/wall.txt" > "$OUT/wall.tmp" && mv "$OUT/wall.tmp" "$OUT/wall.txt"
else
  rm -rf "$OUT" && mkdir -p "$OUT"
fi

now() { python3 -c 'import time; print(time.time())'; }

if [ "${TAPWING_ONLY:-0}" != "1" ]; then
echo "== Maestro $(maestro --version 2>/dev/null | tail -1) on plain APK (no driver)"
adb -s "$DEVICE" install -r bench/apks/plain.apk >/dev/null
for i in $(seq 1 "$RUNS"); do
  s=$(now)
  maestro --device "$DEVICE" test --debug-output "$OUT/maestro-$i" "$FLOW" >"$OUT/maestro-$i.log" 2>&1 || echo "maestro run $i FAILED"
  e=$(now)
  python3 -c "print('maestro run $i: %.1f s' % ($e - $s))" | tee -a "$OUT/wall.txt"
done
fi

echo "== Tapwing on driver APK"
adb -s "$DEVICE" install -r bench/apks/tapwing.apk >/dev/null
for i in $(seq 1 "$RUNS"); do
  s=$(now)
  "$TAPWING" test "$FLOW" --device "$DEVICE" --check-settle --report "$OUT/tapwing-$i.json" >"$OUT/tapwing-$i.log" 2>&1 || echo "tapwing run $i FAILED"
  e=$(now)
  python3 -c "print('tapwing run $i: %.1f s' % ($e - $s))" | tee -a "$OUT/wall.txt"
done

if [ "${TAPWING_ONLY:-0}" != "1" ]; then
echo "== Control: Maestro on the driver APK (driver must not interfere)"
s=$(now)
maestro --device "$DEVICE" test --debug-output "$OUT/maestro-on-driver-apk" "$FLOW" >"$OUT/maestro-on-driver-apk.log" 2>&1 \
  && echo "control: PASSED" | tee -a "$OUT/wall.txt" || echo "control: FAILED" | tee -a "$OUT/wall.txt"
e=$(now)
python3 -c "print('control run: %.1f s' % ($e - $s))" | tee -a "$OUT/wall.txt"
fi

python3 -I bench/tools/compare.py "$OUT" | tee "$OUT/summary.md"

#!/usr/bin/env bash
# iOS spike benchmark: Maestro (plain app) vs Tapwing (driver app), same debug build.
set -uo pipefail
S="$(cd "$(dirname "$0")" && pwd)"
U=47862A4D-744F-4125-8D9E-20D8B51AD7DF
RUNS=${1:-3}
OUT=$S/bench
mkdir -p $OUT
now() { python3 -c 'import time; print(time.time())'; }
if [ "${TAPWING_ONLY:-0}" != "1" ]; then
  rm -rf $OUT/maestro-*; grep -v '^maestro' $OUT/wall.txt > $OUT/w.tmp 2>/dev/null; mv $OUT/w.tmp $OUT/wall.txt 2>/dev/null
  xcrun simctl terminate $U dev.tapwing.sampleApp >/dev/null 2>&1; xcrun simctl install $U $S/apps/plain.app
  for i in $(seq 1 $RUNS); do
    s=$(now); maestro --device $U test --debug-output $OUT/maestro-$i $S/login_and_browse_ios.yaml > $OUT/maestro-$i.log 2>&1 && r=PASSED || r=FAILED; e=$(now)
    python3 -c "print('maestro run $i: %.1f s $r' % ($e - $s))" | tee -a $OUT/wall.txt
  done
fi
rm -f $OUT/tapwing-*; grep -v '^tapwing' $OUT/wall.txt > $OUT/w.tmp 2>/dev/null; mv $OUT/w.tmp $OUT/wall.txt 2>/dev/null
xcrun simctl terminate $U dev.tapwing.sampleApp >/dev/null 2>&1; xcrun simctl install $U $S/apps/tapwing.app
for i in $(seq 1 $RUNS); do
  s=$(now); $S/target/release/tapwing test $S/login_and_browse_ios.yaml --device $U --check-settle --report $OUT/tapwing-$i.json > $OUT/tapwing-$i.log 2>&1 && r=PASSED || r=FAILED; e=$(now)
  python3 -c "print('tapwing run $i: %.1f s $r' % ($e - $s))" | tee -a $OUT/wall.txt
done

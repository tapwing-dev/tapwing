#!/usr/bin/env bash
# Phase 1, experiment 3: capture the runner's raw /viewHierarchy and the probe's tree of the same
# unchanged screen. A tapwing flow (--input native) navigates and then idles in a 60 s optional
# wait, so the runner stays up while this script reads both sides.
# Usage: capture_parity.sh <name> <expected-label> [flow]
set -u
repo=$(cd "$(dirname "$0")/../../../.." && pwd); cd "$repo"
U=32E8A0E5-D41A-4CC4-BCD4-39E9809FAA7D
P=bench/ios/hid-probe/build/hid-probe
name=$1; want=$2; flow=${3:-bench/results/ios-hid-spike/phase1/flows/parity_$1.yaml}
O=bench/results/ios-hid-spike/phase1/exp3; PORT=47311
TAPWING_IOS_NATIVE_PORT=$PORT ./crates/tapwing/target/release/tapwing test "$flow" --device $U --input native > $O/$name.flow.log 2>&1 &
tw=$!
ok=0
for i in $(seq 1 120); do
  $P snapshot $U --out $O/$name.wait.json 2>/dev/null && grep -q "$want" $O/$name.wait.json && ok=1 && break
  sleep 0.5
done
rm -f $O/$name.wait.json
for i in $(seq 1 60); do curl -s -m 2 http://127.0.0.1:$PORT/status >/dev/null && break; sleep 0.5; done
[ $ok = 1 ] || { echo "$name: screen never showed '$want'"; kill -INT $tw; wait $tw; exit 1; }
sleep 2.5   # let animations finish
body='{"appIds":[],"excludeKeyboardElements":false}'
for k in 1 2; do
  curl -s -m 20 -X POST -d "$body" http://127.0.0.1:$PORT/viewHierarchy > $O/$name.runner$k.json
  $P snapshot $U --springboard --out $O/$name.probe$k.json 2> $O/$name.probe$k.err
done
$P snapshot $U --springboard --all-attrs --out $O/$name.probe_allattrs.json 2>/dev/null
$P frame $U $O/$name.png 2>/dev/null
kill -INT $tw; wait $tw 2>/dev/null
sleep 1
echo "$name: runner $(wc -c < $O/$name.runner1.json) bytes, probe $(wc -c < $O/$name.probe1.json) bytes; leftover runner procs: $(pgrep -fl 'xcodebuild' | grep -c $U)"

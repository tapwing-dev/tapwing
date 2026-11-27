#!/usr/bin/env bash
# Phase 2 smoke check of the HID backend (issue #30). Not the benchmark: one run per configuration.
# Usage: smoke.sh <flow> <auto|native> <off|on> [fail-point]
# Logs go to smoke/<flow>-<mode>-hid<off|on>[-fail-<point>].log. While the run lasts, a sampler counts
# xcodebuild processes of the simulator every 100 ms into the .xcb file (max over the run).
set -u
flow=$1; mode=$2; hid=$3; fail=${4:-}
repo=$(cd "$(dirname "$0")/../../../.." && pwd)
dev=32E8A0E5-D41A-4CC4-BCD4-39E9809FAA7D
name=$flow-$mode-hid$hid${fail:+-fail-$fail}
out=$repo/bench/results/ios-hid-spike/phase2/smoke
flags=(); [ "$mode" = native ] && flags=(--input native)
envs=(TAPWING_IOS_NATIVE_TIMING=1)
[ "$hid" = on ] && envs+=(TAPWING_IOS_HID=1)
[ -n "$fail" ] && envs+=(TAPWING_IOS_HID_FAIL=$fail)
( max=0; while [ ! -e "$out/$name.done" ]; do
    n=$(pgrep -fl xcodebuild | grep -c "$dev"); [ "$n" -gt "$max" ] && max=$n; echo $max > "$out/$name.xcb"; sleep 0.1; done ) &
sampler=$!
rm -f "$out/$name.done"
(cd "$repo" && env "${envs[@]}" ./crates/tapwing/target/release/tapwing test \
  "bench/results/ios-hid-spike/flows/$flow.yaml" --device "$dev" ${flags[@]+"${flags[@]}"} \
  --report "$out/$name.json" > "$out/$name.log" 2>&1)
rc=$?
touch "$out/$name.done"; wait $sampler 2>/dev/null; rm -f "$out/$name.done"
echo "$name rc=$rc max_xcodebuild=$(cat "$out/$name.xcb" 2>/dev/null)"
exit $rc

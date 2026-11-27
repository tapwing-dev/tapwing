#!/usr/bin/env bash
# Runs one tapwing flow and records report, log, wall time and the max number of xcodebuild processes seen.
# Usage: run_one.sh <outdir> <name> <flowpath> <auto|native> <off|on> [fail-point]
# Env: DEV (default: main simulator).
set -u
out=$1; name=$2; flow=$3; mode=$4; hid=$5; fail=${6:-}
repo=$(cd "$(dirname "$0")/../../../.." && pwd)
dev=${DEV:-32E8A0E5-D41A-4CC4-BCD4-39E9809FAA7D}
mkdir -p "$out"
flags=(); [ "$mode" = native ] && flags=(--input native)
envs=(TAPWING_IOS_NATIVE_TIMING=1)
[ "$hid" = on ] && envs+=(TAPWING_IOS_HID=1)
[ -n "$fail" ] && envs+=(TAPWING_IOS_HID_FAIL=$fail)
rm -f "$out/$name.done"
( max=0; while [ ! -e "$out/$name.done" ]; do
    n=$(pgrep -fl xcodebuild | grep -c "$dev"); [ "$n" -gt "$max" ] && max=$n; echo $max > "$out/$name.xcb"; sleep 0.1; done ) &
sampler=$!
start=$(perl -MTime::HiRes=time -e 'print time')
(cd "$repo" && env "${envs[@]}" ./crates/tapwing/target/release/tapwing test "$flow" --device "$dev" ${flags[@]+"${flags[@]}"} \
  --report "$out/$name.json" > "$out/$name.log" 2>&1)
rc=$?
end=$(perl -MTime::HiRes=time -e 'print time')
touch "$out/$name.done"; wait $sampler 2>/dev/null; rm -f "$out/$name.done"
echo "$rc $(perl -e "printf '%.3f', $end - $start")" > "$out/$name.wall"
echo "$name rc=$rc wall=$(cut -d' ' -f2 "$out/$name.wall") xcb=$(cat "$out/$name.xcb" 2>/dev/null)"
exit $rc

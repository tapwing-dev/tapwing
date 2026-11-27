#!/usr/bin/env bash
# Step 0 of the spike in #30: measure the iOS native lane on f5_cs / f5_cs_lazy.
# Usage: run_matrix.sh <flow-name> <auto|native>
# Runs 1 discarded warm-up (warmup.*) and 5 measured runs (run-1..5.*) in sequence.
# Stops at the first failed run so the simulator can be checked for a leftover alert.
set -u
flow=$1
mode=$2
repo=$(cd "$(dirname "$0")/../../.." && pwd)
dev=32E8A0E5-D41A-4CC4-BCD4-39E9809FAA7D
out=$repo/bench/results/ios-hid-spike/step0/$flow-$mode
mkdir -p "$out"
flags=()
[ "$mode" = native ] && flags=(--input native)
for i in warmup 1 2 3 4 5; do
  name=$([ "$i" = warmup ] && echo warmup || echo "run-$i")
  start=$(perl -MTime::HiRes=time -e 'print time')
  (cd "$repo" && TAPWING_IOS_NATIVE_TIMING=1 ./crates/tapwing/target/release/tapwing test \
    "bench/results/ios-hid-spike/flows/$flow.yaml" --device "$dev" ${flags[@]+"${flags[@]}"} \
    --report "$out/$name.json" > "$out/$name.log" 2>&1)
  rc=$?
  end=$(perl -MTime::HiRes=time -e 'print time')
  echo "$rc $(perl -e "printf '%.3f', $end - $start")" > "$out/$name.wall"
  echo "$flow $mode $name rc=$rc wall=$(cut -d' ' -f2 "$out/$name.wall")s"
  if [ "$rc" -ne 0 ]; then exit "$rc"; fi
done

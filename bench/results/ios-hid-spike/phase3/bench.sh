#!/usr/bin/env bash
# Task 2: interleaved benchmark. Usage: bench.sh <f5_cs|f5_cs_lazy> <auto|native>
# One discarded warm-up per flag state, then 5 rounds of (off, on). Stops at the first failed run.
set -u
here=$(cd "$(dirname "$0")" && pwd)
flow=$1; mode=$2
out=$here/bench/$flow-$mode
fp=bench/results/ios-hid-spike/flows/$flow.yaml
for h in off on; do "$here/run_one.sh" "$out" warmup-$h "$fp" "$mode" "$h" || exit 1; done
for i in 1 2 3 4 5; do
  for h in off on; do "$here/run_one.sh" "$out" run-$i-$h "$fp" "$mode" "$h" || exit 1; done
done

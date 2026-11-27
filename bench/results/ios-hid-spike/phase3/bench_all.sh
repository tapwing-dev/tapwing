#!/usr/bin/env bash
here=$(cd "$(dirname "$0")" && pwd)
for c in "f5_cs auto" "f5_cs native" "f5_cs_lazy auto" "f5_cs_lazy native"; do
  "$here/bench.sh" $c || { echo "STOPPED at $c"; exit 1; }
done
echo ALLDONE

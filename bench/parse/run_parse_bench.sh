#!/usr/bin/env bash
# Parse-only benchmark: Maestro's YamlCommandReader vs tapwing::flow, no device.
#   bench/parse/run_parse_bench.sh [iterations] [warmup]
# 1. in-process over every flow in compat/maestro-fixtures (cold first pass, then warm)
# 2. a fresh process parsing one flow and exiting, as a CLI does on every run
# Needs java, kotlinc and an installed Maestro (its jars are the reference parser).
set -euo pipefail
cd "$(dirname "$0")/../.."
ITERS="${1:-50}"
WARM="${2:-20}"
B=bench/parse
FIXTURES=compat/maestro-fixtures
FLOW=examples/sample_app/.maestro/login_and_browse.yaml
LIB="${MAESTRO_LIB:-$(dirname "$(dirname "$(python3 -c 'import os,shutil;print(os.path.realpath(shutil.which("maestro")))')")")/lib}"
CP=$(ls "$LIB"/*.jar | tr '\n' ':')
JAR=$B/build/bench.jar
TAPWING=$B/target/release/tapwing-parse-bench

mkdir -p $B/build
if [ ! -f $JAR ] || [ $B/MaestroParseBench.kt -nt $JAR ]; then
  kotlinc $B/MaestroParseBench.kt -cp "$CP" -Xskip-metadata-version-check -d $JAR 2>&1 | grep -v '^warning' || true
fi
cargo build --release --quiet --manifest-path $B/Cargo.toml

echo "== Tapwing, in-process, $FIXTURES ($ITERS iterations, $WARM warmup)"
$TAPWING $FIXTURES "$ITERS" "$WARM"
echo
echo "== Maestro $(maestro --version 2>/dev/null | tail -1), in-process, $FIXTURES"
java -cp "$JAR:$CP" MaestroParseBenchKt $FIXTURES "$ITERS" "$WARM" 2>/dev/null
echo
echo "== fresh process, parse $FLOW and exit"
python3 -I - "$TAPWING" "$JAR:$CP" "$FLOW" <<'EOF'
import statistics, subprocess, sys, time
tapwing, cp, flow = sys.argv[1:4]
def bench(cmd, n):
    subprocess.run(cmd, check=True, capture_output=True)  # page cache
    ts = []
    for _ in range(n):
        a = time.perf_counter()
        subprocess.run(cmd, check=True, capture_output=True)
        ts.append((time.perf_counter() - a) * 1e3)
    return statistics.median(ts), min(ts), max(ts)
k = bench([tapwing, "--once", flow], 50)
m = bench(["java", "-cp", cp, "MaestroParseBenchKt", "--once", flow], 15)
t = bench(["/usr/bin/true"], 50)
print("tapwing  median=%.1f ms min=%.1f max=%.1f (50 runs)" % k)
print("maestro  median=%.1f ms min=%.1f max=%.1f (15 runs)" % m)
print("process spawn baseline (/usr/bin/true) median=%.1f ms" % t[0])
print("speedup (median): %.0fx" % (m[0] / k[0]))
EOF

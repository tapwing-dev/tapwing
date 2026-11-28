#!/usr/bin/env bash
# Browser-login baseline: the sample app's web login flows, N runs per page
# variant and runner, then per-step medians.
#   bench/web-login/run_bench.sh android <adb-serial> [runs]
#   bench/web-login/run_bench.sh ios <simulator-udid> [runs]
#   RUNNERS="tapwing maestro" bench/web-login/run_bench.sh android <serial>   # interleaved, run by run
#   VARIANTS="plain controlled" ...   # default: plain two-step controlled
#   OUT=<dir> ...                     # raw reports and logs (default bench/results/browser-login/<platform>)
#   PORT=8765 ...                     # the port the app was built with (SAMPLE_WEB_LOGIN_URL)
#   MAESTRO_DEVICE_ARGS="--host localhost --port 5559" ...   # instead of `--device <device>`
#   IOS_KEEP_SAFARI_VIEW_SERVICE=1 ...   # iOS: do not restart com.apple.SafariViewService before each run
# One discarded warm-up per variant and runner, then [runs] measured runs (default 5).
# `runs` 0 only warms up, which is a quick check that every flow passes.
#
# Starts bench/web-login/server.py and, on Android, `adb reverse` for its port.
# Both are removed on exit. Needs the sample app installed (Android profile APK
# or iOS Simulator debug build of lib/main_tapwing.dart, see CONTRIBUTING.md);
# Maestro runs against that same build. Only summary.md is meant to be committed.
set -euo pipefail
cd "$(dirname "$0")/../.."
PLATFORM="${1:?android or ios}"
DEVICE="${2:?adb serial or simulator UDID}"
RUNS="${3:-5}"
RUNNERS="${RUNNERS:-tapwing}"
VARIANTS="${VARIANTS:-plain two-step controlled}"
PORT="${PORT:-8765}"
OUT="${OUT:-bench/results/browser-login/$PLATFORM}"
TAPWING="${TAPWING_BIN:-./crates/tapwing/target/release/tapwing}"
# Maestro 2.12.1 lists no Android device at all while any attached device is
# `unauthorized`. Its `--host`/`--port` pair connects to one emulator's adb port
# directly (console port + 1) and does not need the listing.
MAESTRO_DEVICE_ARGS="${MAESTRO_DEVICE_ARGS:---device $DEVICE}"
case "$PLATFORM" in
  android) FLOWS=examples/sample_app/.maestro/web ;;
  ios) FLOWS=examples/sample_app/.maestro/ios/web ;;
  *) echo "platform must be android or ios, got $PLATFORM" >&2; exit 2 ;;
esac
rm -rf "$OUT" && mkdir -p "$OUT"
now() { python3 -c 'import time; print(time.time())'; }

python3 -I bench/web-login/server.py --port "$PORT" >>"$OUT/server.log" 2>&1 &
server_pid=$!
cleanup() {
  kill "$server_pid" 2>/dev/null || true
  if [ "$PLATFORM" = android ]; then adb -s "$DEVICE" reverse --remove "tcp:$PORT" >/dev/null 2>&1 || true; fi
}
trap cleanup EXIT
for _ in $(seq 1 50); do
  curl -fs -m 1 "http://127.0.0.1:$PORT/healthz" >/dev/null 2>&1 && break
  kill -0 "$server_pid" 2>/dev/null || { echo "the login server did not start, see $OUT/server.log" >&2; exit 2; }
  python3 -c 'import time; time.sleep(0.1)'
done
curl -fs -m 1 "http://127.0.0.1:$PORT/healthz" >/dev/null || { echo "no login server on port $PORT" >&2; exit 2; }

# run_flow <runner> <variant> <label>: one run, one line in runs.tsv.
run_flow() {
  local runner="$1" variant="$2" label="$3" status=passed s e
  local flow="$FLOWS/web_login_${variant//-/_}.yaml"
  local base="$OUT/$runner-$variant-$label"
  # A marker, so the server's lines (event counts, rejected logins) belong to a run.
  echo "run runner=$runner variant=$variant run=$label" >>"$OUT/server.log"
  if [ "$PLATFORM" = android ]; then adb -s "$DEVICE" reverse "tcp:$PORT" "tcp:$PORT" >/dev/null; fi
  # iOS 27.2 Simulator: after about four sign-in sheets the accessibility server
  # of com.apple.SafariViewService stops answering XCTest (kAXErrorServerNotFound)
  # and neither runner sees the page any more. A fresh process per run avoids that.
  if [ "$PLATFORM" = ios ] && [ "${IOS_KEEP_SAFARI_VIEW_SERVICE:-0}" != 1 ]; then
    xcrun simctl terminate "$DEVICE" com.apple.SafariViewService >/dev/null 2>&1 || true
  fi
  s=$(now)
  case "$runner" in
    tapwing)
      "$TAPWING" test "$flow" --device "$DEVICE" --input auto --report "$base.json" \
        --test-output-dir "$base.out" >"$base.log" 2>&1 || status=failed ;;
    maestro)
      # Word splitting is wanted: the variable holds flags.
      # shellcheck disable=SC2086
      MAESTRO_CLI_NO_ANALYTICS=1 maestro $MAESTRO_DEVICE_ARGS test --debug-output "$base" "$flow" \
        >"$base.log" 2>&1 || status=failed ;;
    *) echo "unknown runner $runner" >&2; exit 2 ;;
  esac
  e=$(now)
  python3 -c "import os; print('$runner\t$variant\t$label\t$status\t%.1f\t%.1f' % ($e - $s, os.getloadavg()[0]))" \
    | tee -a "$OUT/runs.tsv"
}

for variant in $VARIANTS; do
  echo "== $variant: warm-up (discarded)"
  for runner in $RUNNERS; do run_flow "$runner" "$variant" warmup; done
  [ "$RUNS" -gt 0 ] || continue
  echo "== $variant: $RUNS runs"
  for i in $(seq 1 "$RUNS"); do
    for runner in $RUNNERS; do run_flow "$runner" "$variant" "$i"; done
  done
done

python3 -I bench/web-login/summarize.py "$OUT" | tee "$OUT/summary.md"

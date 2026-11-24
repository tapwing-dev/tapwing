#!/usr/bin/env bash
# Hierarchy dumps of a set of screens through Tapwing's Android driver
# (crates/tapwing/examples/native_hierarchy_diff.rs), with timings and a stability
# verdict. Results: bench/results/f11a/hierarchy/<screen>.{tapwing.xml,json}
# (or $PARITY_OUT). F11a compared these dumps with Maestro 2.12.1's driver
# before it was removed; the saved results in bench/results/f11a/ keep that comparison.
#
#   bench/f11a/run_parity.sh <adb-serial> [screen ...]
#
# (The open-source licenses WebView, `android.settings.LICENSE`, is left out: one
# dump of it takes longer than the 30 s RPC deadline.)
#
# Needs the sample app installed (examples/sample_app, profile build with
# lib/main_tapwing.dart) and Tapwing's driver (a release bundle, $TAPWING_DRIVER_APK
# or drivers/android/build/tapwing-driver.apk from scripts/build-android-driver.sh).
# Run it on a device you own: it clears the sample app's data, opens Settings and
# Chrome, and expands the notification shade, whose dumps contain what is on them.
set -euo pipefail
SERIAL=${1:?adb serial}; shift
ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
OUT="${PARITY_OUT:-$ROOT/bench/results/f11a/hierarchy}"
FLOWS="$ROOT/bench/f11a/flows"
CRATE="$ROOT/crates/tapwing"
(cd "$CRATE" && cargo build -q --release --bin tapwing --example native_hierarchy_diff)
TAPWING="$CRATE/target/release/tapwing"
HARNESS="$CRATE/target/release/examples/native_hierarchy_diff"
adb() { command adb -s "$SERIAL" "$@"; }
settle() { sleep "${1:-3}"; }
flow() { "$TAPWING" test --device "$SERIAL" --input flutter "$FLOWS/$1.yaml" >/dev/null; settle 2; }
home() { adb shell input keyevent 3; settle; }
collapse() { adb shell cmd statusbar collapse; }

setup() {
  collapse
  case "$1" in
    # The hotseat search bar keeps moving for a few seconds after Home.
    launcher) home; settle 6 ;;
    settings) adb shell am force-stop com.android.settings; adb shell am start -W -n com.android.settings/.Settings >/dev/null; settle ;;
    settings_display) adb shell am start -W -a android.settings.DISPLAY_SETTINGS >/dev/null; settle ;;
    chrome_page) adb shell am start -W -a android.intent.action.VIEW -d https://example.com/ com.android.chrome >/dev/null; settle 8 ;;
    # A local page with one <input> (served by this script, see web/).
    chrome_web_input) adb shell am start -W -a android.intent.action.VIEW -d http://127.0.0.1:8099/ com.android.chrome >/dev/null; settle 6 ;;
    notification_shade) home; adb shell cmd statusbar expand-notifications; settle ;;
    quick_settings) home; adb shell cmd statusbar expand-settings; settle ;;
    sample_login) adb shell pm clear dev.tapwing.sample_app >/dev/null; adb shell am start -W -n dev.tapwing.sample_app/.MainActivity >/dev/null; settle 4 ;;
    sample_playground) flow to_playground ;;
    sample_text_keyboard) flow to_text_keys_keyboard ;;
    permission_dialog) flow to_permission_dialog ;;
    *) echo "unknown screen $1" >&2; return 1 ;;
  esac
}

# The page for chrome_web_input, reachable on the device's 127.0.0.1:8099.
python3 -I -m http.server 8099 --bind 127.0.0.1 --directory "$ROOT/bench/f11a/web" >/dev/null 2>&1 &
web_pid=$!
trap 'kill $web_pid 2>/dev/null' EXIT
adb reverse tcp:8099 tcp:8099 >/dev/null

screens=("$@")
[[ ${#screens[@]} -gt 0 ]] || screens=(launcher settings settings_display chrome_page chrome_web_input \
  notification_shade quick_settings sample_login sample_playground sample_text_keyboard permission_dialog)
mkdir -p "$OUT"
for s in "${screens[@]}"; do
  setup "$s"
  "$HARNESS" dump --serial "$SERIAL" --out "$OUT" --label "$s" | grep -E "ready|stable"
done
collapse

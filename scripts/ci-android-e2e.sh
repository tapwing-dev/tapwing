#!/usr/bin/env bash
# End-to-end run of the sample app's Android flows on one adb device: the CLI,
# the in-app Dart driver and the native driver APK together.
#
#   scripts/ci-android-e2e.sh --serial emulator-5554                # build, then run
#   scripts/ci-android-e2e.sh --build-only --abi x86_64             # build without a device
#   scripts/ci-android-e2e.sh --skip-build --serial emulator-5554   # run what is already built
#
# Options:
#   --serial <adb serial>  device to run on (default: $ANDROID_SERIAL). The script never
#                          picks a device on its own.
#   --abi <abi>            ABI of the sample app APK: arm64-v8a, x86_64 or armeabi-v7a
#                          (default: the device's ABI)
#   --out <dir>            reports, traces and logcat (default: build/android-e2e)
#   --build-only           stop after the builds
#   --skip-build           use the artifacts of an earlier build
#
# Build: the release `tapwing` binary, the driver APK (scripts/build-android-driver.sh)
# and the sample app profile APK with `-t lib/main_tapwing.dart`.
#
# Run, each as its own `tapwing test` call with `--trace`, a JUnit and a JSON report:
#   1. positive   every top-level flow of examples/sample_app/.maestro in one call,
#                 except NIGHT_MODE_FLOWS and EXCLUDED_FLOWS below. Must exit 0.
#   2. night      each flow of NIGHT_MODE_FLOWS, after the script switched the device
#                 to night mode. Must exit 0.
#   3. negative   each flow of .maestro/negative/. Must exit 1: the flow failed. Exit 2
#                 is a runner or device error and fails this script.
#
# Flows are passed to `tapwing test` as files, so a config.yaml added to .maestro later
# cannot widen the selection. No other subdirectory runs here: ios/ needs an iOS
# Simulator, web/ needs a web server on the host.
#
# The script sets what the flows depend on and restores it on exit: display density
# (TAPWING_E2E_DENSITY, default 560, empty keeps the device's own), night mode and the
# system's error dialogs, which it hides (see "System error dialogs" below). It checks
# the system locale and fails when it is not English.
#
# Every build and run has a time limit and fails with "TIMEOUT: <step>". Exit code: 0
# when every run ended as expected, 1 otherwise, 2 for a usage error.
#
# Needs: adb, cargo and protoc, flutter, JDK 17+, an Android SDK with platform 37.
set -euo pipefail

ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
APP="$ROOT/examples/sample_app"
FLOWS="$APP/.maestro"
APP_ID=dev.tapwing.sample_app
TAPWING="$ROOT/crates/tapwing/target/release/tapwing"
DRIVER_APK="$ROOT/drivers/android/build/tapwing-driver.apk"
APP_APK="$APP/build/app/outputs/flutter-apk/app-profile.apk"

# Top-level flows that need the device in night mode before `launchApp`. The app
# follows the system theme and these flows do not switch it themselves, so they
# cannot share a run with the flows that assert the light theme.
NIGHT_MODE_FLOWS=(
  f2_dark_mode.yaml
)

# Top-level flows this script does not run, one "<file>|<reason>" per line. Every
# entry is printed in the log and in the summary. Empty: all top-level flows run.
EXCLUDED_FLOWS=(
)

# The reference PNGs of f2_screenshots_and_theme were recorded at 560 dpi.
DENSITY=${TAPWING_E2E_DENSITY-560}

# Time limits in seconds.
T_CARGO=1500
T_DRIVER=900
T_FLUTTER=1500
T_DEVICE=180
T_INSTALL=300
T_POSITIVE=1200
T_FLOW=300

log() { echo "ci-android-e2e: $*"; }
die() { echo "ci-android-e2e: $*" >&2; exit 1; }
usage_error() { echo "ci-android-e2e: $*" >&2; exit 2; }

serial=${ANDROID_SERIAL:-}
abi=
out="$ROOT/build/android-e2e"
do_build=1
do_run=1
while [[ $# -gt 0 ]]; do
  case "$1" in
    --serial) [[ $# -ge 2 ]] || usage_error "--serial needs a value"; serial=$2; shift 2 ;;
    --abi) [[ $# -ge 2 ]] || usage_error "--abi needs a value"; abi=$2; shift 2 ;;
    --out) [[ $# -ge 2 ]] || usage_error "--out needs a value"; out=$2; shift 2 ;;
    --build-only) do_run=0; shift ;;
    --skip-build) do_build=0; shift ;;
    -h|--help) awk 'NR > 1 && /^#/ { sub(/^# ?/, ""); print; next } NR > 1 { exit }' "$0"; exit 0 ;;
    *) usage_error "unknown argument: $1" ;;
  esac
done
[[ $do_build -eq 1 || $do_run -eq 1 ]] || usage_error "--build-only and --skip-build leave nothing to do"
[[ $do_run -eq 0 || -n "$serial" ]] || usage_error "no device: pass --serial <adb serial> or set ANDROID_SERIAL"
[[ $do_run -eq 1 || -n "$abi" ]] || usage_error "--build-only needs --abi (there is no device to read it from)"

mkdir -p "$out"
out=$(cd "$out" && pwd)

# --- Time limits -------------------------------------------------------------

child=
logcat_pid=
restore_night=
restore_density=
restore_error_dialogs=
timeout_marker="$out/.timeout"

# run_step <seconds> <label> <command...>
# Returns the command's exit code, or 124 after "TIMEOUT: <label>" when it ran longer
# than <seconds>. The command gets SIGTERM first (`tapwing test` then restores the
# device and writes a partial report) and SIGKILL 30 s later.
run_step() {
  local secs=$1 label=$2 rc=0 watchdog
  shift 2
  rm -f "$timeout_marker"
  "$@" &
  child=$!
  # `|| exit 0`: stopping the watchdog below kills its `sleep` first, and the watchdog
  # must then end without marking a timeout or signalling a finished command.
  (
    sleep "$secs" || exit 0
    : >"$timeout_marker"
    kill -TERM "$child" 2>/dev/null
    sleep 30 || exit 0
    kill -KILL "$child" 2>/dev/null
  ) </dev/null >/dev/null 2>&1 &
  watchdog=$!
  wait "$child" || rc=$?
  child=
  pkill -P "$watchdog" 2>/dev/null || true
  kill "$watchdog" 2>/dev/null || true
  wait "$watchdog" 2>/dev/null || true
  if [[ -f "$timeout_marker" ]]; then
    rm -f "$timeout_marker"
    echo "ci-android-e2e: TIMEOUT: $label did not finish within ${secs}s" >&2
    return 124
  fi
  return "$rc"
}

# must <seconds> <label> <command...>: run_step, and any failure ends the script.
must() {
  local rc=0
  run_step "$@" || rc=$?
  [[ $rc -eq 0 ]] && return 0
  [[ $rc -eq 124 ]] && exit 1
  die "$2 failed (exit $rc)"
}

adb_dev() { adb -s "$serial" "$@"; }
# One short device command with its own limit, so a hung adb cannot stall the script.
dev_sh() { run_step 30 "adb shell $*" adb -s "$serial" shell "$@"; }

cleanup() {
  local rc=$?
  trap - EXIT INT TERM
  if [[ -n "$child" ]]; then kill -TERM "$child" 2>/dev/null || true; fi
  if [[ -n "$restore_night" ]]; then
    dev_sh cmd uimode night "$restore_night" >/dev/null 2>&1 || true
  fi
  if [[ -n "$restore_density" ]]; then
    # shellcheck disable=SC2086 # "reset" or a number, one word either way
    dev_sh wm density $restore_density >/dev/null 2>&1 || true
  fi
  if [[ "$restore_error_dialogs" == null ]]; then
    dev_sh settings delete global hide_error_dialogs >/dev/null 2>&1 || true
  elif [[ -n "$restore_error_dialogs" ]]; then
    dev_sh settings put global hide_error_dialogs "$restore_error_dialogs" >/dev/null 2>&1 || true
  fi
  if [[ -n "$logcat_pid" ]]; then kill "$logcat_pid" 2>/dev/null || true; fi
  rm -f "$timeout_marker"
  # Anything that is not success or a signal is a failed run, whatever command reported it.
  case "$rc" in 0|130|143) ;; *) rc=1 ;; esac
  exit "$rc"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

# --- Flow selection ------------------------------------------------------------

excluded_reason() { # <file>: prints the reason and returns 0 when the flow is excluded
  local entry
  for entry in ${EXCLUDED_FLOWS[@]+"${EXCLUDED_FLOWS[@]}"}; do
    if [[ "${entry%%|*}" == "$1" ]]; then echo "${entry#*|}"; return 0; fi
  done
  return 1
}

is_night_flow() {
  local name
  for name in ${NIGHT_MODE_FLOWS[@]+"${NIGHT_MODE_FLOWS[@]}"}; do
    [[ "$name" == "$1" ]] && return 0
  done
  return 1
}

# A renamed flow must not leave a stale entry behind.
for entry in ${EXCLUDED_FLOWS[@]+"${EXCLUDED_FLOWS[@]}"}; do
  [[ "$entry" == *"|"?* ]] || die "EXCLUDED_FLOWS entry without a reason: $entry"
  [[ -f "$FLOWS/${entry%%|*}" ]] || die "EXCLUDED_FLOWS names a flow that does not exist: ${entry%%|*}"
done
for name in ${NIGHT_MODE_FLOWS[@]+"${NIGHT_MODE_FLOWS[@]}"}; do
  [[ -f "$FLOWS/$name" ]] || die "NIGHT_MODE_FLOWS names a flow that does not exist: $name"
  if excluded_reason "$name" >/dev/null; then die "$name is in both NIGHT_MODE_FLOWS and EXCLUDED_FLOWS"; fi
done

positive=()
night=()
negative=()
skipped=()
for path in "$FLOWS"/*.yaml "$FLOWS"/*.yml; do
  [[ -f "$path" ]] || continue
  name=$(basename "$path")
  case "$name" in config.yaml|config.yml) continue ;; esac
  if reason=$(excluded_reason "$name"); then
    skipped+=("$name|$reason")
  elif is_night_flow "$name"; then
    night+=("$path")
  else
    positive+=("$path")
  fi
done
for path in "$FLOWS"/negative/*.yaml "$FLOWS"/negative/*.yml; do
  [[ -f "$path" ]] && negative+=("$path")
done
[[ ${#positive[@]} -gt 0 ]] || die "no top-level flows in $FLOWS"
[[ ${#negative[@]} -gt 0 ]] || die "no flows in $FLOWS/negative"

# --- Device ----------------------------------------------------------------------

device_abi=
if [[ $do_run -eq 1 ]]; then
  command -v adb >/dev/null || die "adb not found on PATH"
  state=$(run_step 30 "adb get-state" adb -s "$serial" get-state 2>&1) || true
  case "$state" in
    device) ;;
    *unauthorized*) die "device $serial is unauthorized: accept the USB debugging prompt on it" ;;
    *) die "device $serial is not ready (adb get-state: ${state:-no answer})" ;;
  esac

  log "waiting for $serial to finish booting (limit ${T_DEVICE}s)"
  deadline=$((SECONDS + T_DEVICE))
  until [[ "$(dev_sh getprop sys.boot_completed 2>/dev/null | tr -d '\r')" == "1" ]]; do
    [[ $SECONDS -lt $deadline ]] || die "TIMEOUT: $serial did not report sys.boot_completed=1 within ${T_DEVICE}s"
    sleep 2
  done

  device_abi=$(dev_sh getprop ro.product.cpu.abi | tr -d '\r')
  [[ -n "$device_abi" ]] || die "could not read ro.product.cpu.abi from $serial"
  [[ -z "$abi" || "$abi" == "$device_abi" ]] || die "--abi $abi does not match the device ABI $device_abi"
  abi=$device_abi
fi

case "$abi" in
  arm64-v8a) target_platform=android-arm64 ;;
  x86_64) target_platform=android-x64 ;;
  armeabi-v7a) target_platform=android-arm ;;
  *) die "unsupported ABI for a Flutter profile build: $abi" ;;
esac

# --- Build -----------------------------------------------------------------------

build_app() {
  cd "$APP"
  flutter pub get
  flutter build apk --profile --target-platform "$target_platform" -t lib/main_tapwing.dart
}

if [[ $do_build -eq 1 ]]; then
  started=$SECONDS
  log "building the release tapwing binary (limit ${T_CARGO}s)"
  must "$T_CARGO" "cargo build --release" \
    cargo build --release --locked --manifest-path "$ROOT/crates/tapwing/Cargo.toml" --bin tapwing
  log "building the driver APK (limit ${T_DRIVER}s)"
  must "$T_DRIVER" "scripts/build-android-driver.sh" "$ROOT/scripts/build-android-driver.sh" --no-test
  log "building the sample app profile APK for $abi (limit ${T_FLUTTER}s)"
  must "$T_FLUTTER" "flutter build apk" build_app
  log "builds finished in $((SECONDS - started))s"
fi

[[ -x "$TAPWING" ]] || die "missing $TAPWING (run without --skip-build)"
[[ -f "$DRIVER_APK" ]] || die "missing $DRIVER_APK (run without --skip-build)"
[[ -f "$APP_APK" ]] || die "missing $APP_APK (run without --skip-build)"
# A profile APK holds the AOT code of one ABI. An APK for another ABI installs and then crashes on start.
# Listed into a variable first: `grep -q` closing the pipe early would fail `unzip` under pipefail.
apk_listing=$(unzip -l "$APP_APK")
grep -q " lib/$abi/libapp\.so\$" <<<"$apk_listing" \
  || die "$APP_APK has no lib/$abi/libapp.so: it was built for another ABI"

if [[ $do_run -eq 0 ]]; then
  log "build only: done"
  exit 0
fi

# --- Device state ----------------------------------------------------------------

# The checkout build of the CLI finds this APK on its own. Set it anyway so a stale
# TAPWING_DRIVER_APK from the caller's environment cannot replace it.
export TAPWING_DRIVER_APK="$DRIVER_APK"

locale=$(dev_sh settings get system system_locales | tr -d '\r')
[[ -n "$locale" && "$locale" != "null" ]] || locale=$(dev_sh getprop persist.sys.locale | tr -d '\r')
[[ -n "$locale" ]] || locale=$(dev_sh getprop ro.product.locale | tr -d '\r')
case "$locale" in
  en|en-*|en_*) ;;
  *) die "system locale of $serial is '$locale': f5_permission_dialog taps the English labels of the system permission dialog" ;;
esac

night_now=$(dev_sh cmd uimode night | tr -d '\r' | sed -n 's/^Night mode: //p')
case "$night_now" in
  yes|no|auto) ;;
  *) die "could not read night mode from $serial (cmd uimode night: '$night_now')" ;;
esac

# set_night <yes|no>: switches the device and checks the answer, because some AVDs ignore the command.
set_night() {
  local now
  restore_night=$night_now
  dev_sh cmd uimode night "$1" >/dev/null
  now=$(dev_sh cmd uimode night | tr -d '\r' | sed -n 's/^Night mode: //p')
  [[ "$now" == "$1" ]] || die "$serial ignored 'cmd uimode night $1' (it reports '$now')"
}

density_info=$(dev_sh wm density | tr -d '\r')
physical_density=$(echo "$density_info" | sed -n 's/^Physical density: //p')
override_density=$(echo "$density_info" | sed -n 's/^Override density: //p')
current_density=${override_density:-$physical_density}
[[ -n "$current_density" ]] || die "could not read the display density of $serial (wm density: '$density_info')"
if [[ -n "$DENSITY" && "$current_density" != "$DENSITY" ]]; then
  log "display density $current_density -> $DENSITY for this run (restored on exit)"
  restore_density=${override_density:-reset}
  dev_sh wm density "$DENSITY" >/dev/null
  current_density=$DENSITY
fi

# System error dialogs. On a software-rendered emulator another app can stop responding
# in the middle of a run: the first CI run of this script on a pull request lost 9 of 12
# flows to a "Pixel Launcher isn't responding" dialog that covered the sample app. With
# the dialogs hidden Android kills a process that stops responding instead of asking, so
# nothing covers the screen, and an ANR of the sample app itself still fails its flow,
# as a crash. Every ANR of the run is listed in the summary.
error_dialogs_now=$(dev_sh settings get global hide_error_dialogs | tr -d '\r')
if [[ "$error_dialogs_now" != 1 ]]; then
  log "hiding system error dialogs for this run (restored on exit)"
  restore_error_dialogs=${error_dialogs_now:-null}
  dev_sh settings put global hide_error_dialogs 1 >/dev/null
fi
# The setting does not close a dialog that is already open.
dev_sh am broadcast -a android.intent.action.CLOSE_SYSTEM_DIALOGS >/dev/null
windows=$(dev_sh dumpsys window | tr -d '\r')
focus=$(sed -n 's/^ *mCurrentFocus=//p' <<<"$windows")
case "$focus" in
  *"Application Not Responding"*|*"Application Error"*)
    die "a system error dialog is still open on $serial and would cover the app: $focus" ;;
esac

{
  echo "serial: $serial"
  echo "api: $(dev_sh getprop ro.build.version.sdk | tr -d '\r')"
  echo "abi: $abi"
  echo "model: $(dev_sh getprop ro.product.model | tr -d '\r')"
  echo "size: $(dev_sh wm size | tr -d '\r' | tr '\n' ' ')"
  echo "density: $current_density (physical $physical_density)"
  echo "locale: $locale"
  echo "night mode before the run: $night_now"
  echo "tapwing: $("$TAPWING" --version | sed -n 1p)"
  echo "flutter: $(flutter --version 2>/dev/null | sed -n 's/^Flutter \([^ ]*\).*/\1/p' || true)"
} >"$out/device.txt"
sed 's/^/ci-android-e2e:   /' "$out/device.txt"

dev_sh logcat -c >/dev/null 2>&1 || true
adb_dev logcat -v threadtime >"$out/logcat.txt" 2>&1 &
logcat_pid=$!

log "installing the sample app (limit ${T_INSTALL}s)"
# Uninstall first: an APK signed with another machine's debug key cannot replace this one.
run_step 60 "adb uninstall" adb -s "$serial" uninstall "$APP_ID" >/dev/null 2>&1 || true
must "$T_INSTALL" "adb install" adb -s "$serial" install -r "$APP_APK"

# --- Runs ------------------------------------------------------------------------

results=()
failures=0

# run_flows <label> <expected exit code> <limit> <flow files...>
run_flows() {
  local label=$1 expected=$2 limit=$3 rc=0 started=$SECONDS verdict dir
  shift 3
  dir="$out/$label"
  rm -rf "$dir"
  mkdir -p "$dir"
  echo
  log "== $label: $# flow(s), expecting exit $expected (limit ${limit}s)"
  run_step "$limit" "tapwing test ($label)" "$TAPWING" test --device "$serial" \
    --trace --format junit --output "$dir/junit.xml" --report "$dir/report.json" \
    --test-output-dir "$dir" --test-suite-name "Android E2E: $label" "$@" || rc=$?
  if [[ $rc -eq "$expected" ]]; then
    verdict=ok
  else
    failures=$((failures + 1))
    case "$rc" in
      124) verdict="FAILED: timeout after ${limit}s" ;;
      2) verdict="FAILED: exit 2 is a runner or device error, expected $expected" ;;
      0) verdict="FAILED: the flow passed, expected exit $expected" ;;
      *) verdict="FAILED: exit $rc, expected $expected" ;;
    esac
    echo "ci-android-e2e: $label: $verdict" >&2
  fi
  results+=("$label|$#|$expected|$rc|$((SECONDS - started))s|$verdict")
  # Every later run would fail the same way, so name the cause once.
  [[ "$(run_step 30 "adb get-state" adb -s "$serial" get-state 2>/dev/null)" == "device" ]] \
    || die "device $serial went away during '$label'"
}

run_started=$SECONDS
log "top-level flows in one run: ${#positive[@]}"
for path in "${positive[@]}"; do log "  $(basename "$path")"; done
for entry in ${skipped[@]+"${skipped[@]}"}; do log "  EXCLUDED ${entry%%|*}: ${entry#*|}"; done

set_night no
run_flows positive 0 "$T_POSITIVE" "${positive[@]}"

if [[ ${#night[@]} -gt 0 ]]; then
  set_night yes
  for path in "${night[@]}"; do
    run_flows "night/$(basename "${path%.*}")" 0 "$T_FLOW" "$path"
  done
  set_night no
fi

for path in "${negative[@]}"; do
  run_flows "negative/$(basename "${path%.*}")" 1 "$T_FLOW" "$path"
done

# --- Summary ---------------------------------------------------------------------

{
  echo "| Run | Flows | Expected exit | Exit | Time | Result |"
  echo "|---|---|---|---|---|---|"
  for row in "${results[@]}"; do echo "| ${row//|/ | } |"; done
  for entry in ${skipped[@]+"${skipped[@]}"}; do echo "| ${entry%%|*} | 1 | | not run | | EXCLUDED: ${entry#*|} |"; done
  echo
  echo "Device: $(tr '\n' ';' <"$out/device.txt" | sed 's/;$//; s/;/, /g')"
  echo "Runs took $((SECONDS - run_started))s."
  echo
  # Hidden error dialogs must not hide the ANR itself: name every process that stopped responding.
  anrs=$(sed -n 's/.* ActivityManager: ANR in \([^ ]*\).*/\1/p' "$out/logcat.txt" | sort | uniq -c | sed 's/^ *\([0-9]*\) \(.*\)/\2 (\1x)/' | paste -sd, - | sed 's/,/, /g')
  echo "ANRs during the run: ${anrs:-none}"
} >"$out/summary.md"
echo
cat "$out/summary.md"
if [[ -n "${GITHUB_STEP_SUMMARY:-}" ]]; then
  { echo "### Android emulator E2E"; echo; cat "$out/summary.md"; } >>"$GITHUB_STEP_SUMMARY"
fi
log "reports, traces and logcat: $out"

[[ $failures -eq 0 ]] || die "$failures run(s) did not end as expected"
log "all runs ended as expected"

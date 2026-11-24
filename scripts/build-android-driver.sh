#!/usr/bin/env bash
# Builds Tapwing's own Android native-lane driver (drivers/android) and prints
# its SHA-256.
#
#   scripts/build-android-driver.sh            # unit tests + release APK
#   scripts/build-android-driver.sh --no-test  # APK only
#
# Output: drivers/android/build/tapwing-driver.apk and tapwing-driver.apk.sha256
# (`shasum -a 256 -c` format). A dev build of `tapwing` finds the APK there; a
# release tarball ships it as share/tapwing/drivers/android/tapwing-driver.apk.
#
# Needs JDK 17+ (JAVA_HOME, else /usr/libexec/java_home -v 17 on macOS) and an
# Android SDK with platform 37 (ANDROID_HOME / ANDROID_SDK_ROOT or
# drivers/android/local.properties). Gradle itself comes from the wrapper,
# pinned by SHA-256 in gradle/wrapper/gradle-wrapper.properties. The APK is
# signed with the committed, intentionally public test key
# (drivers/android/signing/README.md), so the same sources give the same APK.
set -euo pipefail

ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
PROJECT="$ROOT/drivers/android"
OUT="$PROJECT/build"

die() { echo "build-android-driver: $*" >&2; exit 1; }

run_tests=1
while [[ $# -gt 0 ]]; do
  case "$1" in
    --no-test) run_tests=0; shift ;;
    -h|--help) awk 'NR > 1 && /^#/ { sub(/^# ?/, ""); print; next } NR > 1 { exit }' "$0"; exit 0 ;;
    *) die "unknown argument: $1" ;;
  esac
done

if [[ -z "${JAVA_HOME:-}" && -x /usr/libexec/java_home ]]; then
  JAVA_HOME=$(/usr/libexec/java_home -v 17 2>/dev/null || true)
  export JAVA_HOME
fi
if [[ -z "${ANDROID_HOME:-}${ANDROID_SDK_ROOT:-}" && ! -f "$PROJECT/local.properties" ]]; then
  die "no Android SDK: set ANDROID_HOME or write sdk.dir to $PROJECT/local.properties"
fi

tasks=(:driver:assembleRelease)
[[ $run_tests -eq 1 ]] && tasks=(:driver:test "${tasks[@]}")
(cd "$PROJECT" && ./gradlew --no-daemon --quiet "${tasks[@]}")

apk="$PROJECT/driver/build/outputs/apk/release/driver-release.apk"
[[ -f "$apk" ]] || die "Gradle did not produce $apk"
mkdir -p "$OUT"
cp "$apk" "$OUT/tapwing-driver.apk"
if command -v sha256sum >/dev/null; then sum=$(sha256sum "$OUT/tapwing-driver.apk" | cut -d' ' -f1)
else sum=$(shasum -a 256 "$OUT/tapwing-driver.apk" | cut -d' ' -f1); fi
echo "$sum  tapwing-driver.apk" >"$OUT/tapwing-driver.apk.sha256"
echo "$OUT/tapwing-driver.apk"
echo "sha256 $sum"

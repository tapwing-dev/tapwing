#!/usr/bin/env bash
# Builds Tapwing's own iOS Simulator native-lane driver (drivers/ios) and writes
# its SHA-256 sidecars.
#
#   scripts/build-ios-driver.sh            # host unit tests + simulator runner bundle
#   scripts/build-ios-driver.sh --no-test  # simulator runner bundle only
#
# Output in drivers/ios/build/:
#   tapwing-ios-runner.xctestrun (+ .sha256)
#   tapwing-ios-runnerUITests-Runner.zip (+ .sha256)
#   tapwing-ios-runner.zip (+ .sha256)
#   tapwing-ios-runner.build-info.json (+ .sha256): the Xcode and SDK the runner was built with
#   Debug-iphonesimulator/{TapwingRunnerUITests-Runner.app,TapwingRunner.app}
#
# A dev build of `tapwing` finds the build directly in drivers/ios/build/; a
# macOS release tarball ships the xctestrun + zips (+ .sha256) under
# share/tapwing/drivers/ios/.
#
# Needs macOS with Xcode (`xcodebuild` and `swift`). Zero external Swift
# packages or CocoaPods are required.
set -euo pipefail

ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
PROJECT="$ROOT/drivers/ios"
OUT="$PROJECT/build"

die() { echo "build-ios-driver: $*" >&2; exit 1; }

run_tests=1
while [[ $# -gt 0 ]]; do
  case "$1" in
    --no-test) run_tests=0; shift ;;
    -h|--help) awk 'NR > 1 && /^#/ { sub(/^# ?/, ""); print; next } NR > 1 { exit }' "$0"; exit 0 ;;
    *) die "unknown argument: $1" ;;
  esac
done

[[ "$(uname -s)" == "Darwin" ]] || die "the iOS Simulator driver can only be built on macOS"
command -v xcodebuild >/dev/null || die "xcodebuild is required (install Xcode)"
command -v swift >/dev/null || die "swift is required (install Xcode)"
command -v zip >/dev/null || die "zip is required"
command -v python3 >/dev/null || die "python3 is required"

sha256() {
  if command -v sha256sum >/dev/null; then sha256sum "$1" | cut -d' ' -f1; else shasum -a 256 "$1" | cut -d' ' -f1; fi
}

if [[ $run_tests -eq 1 ]]; then
  echo "build-ios-driver: running Core unit tests..." >&2
  swift test --package-path "$PROJECT" --quiet
fi

WORK=$(mktemp -d "${TMPDIR:-/tmp}/tapwing-ios-driver-build.XXXXXX")
trap 'rm -rf "$WORK"' EXIT

DERIVED="$WORK/derived"
echo "build-ios-driver: building TapwingRunner for iphonesimulator..." >&2
xcodebuild build-for-testing \
  -project "$PROJECT/TapwingRunner.xcodeproj" \
  -scheme TapwingRunner \
  -sdk iphonesimulator \
  -configuration Debug \
  -destination 'generic/platform=iOS Simulator' \
  -derivedDataPath "$DERIVED" \
  CODE_SIGNING_ALLOWED=NO \
  -quiet

PRODUCTS="$DERIVED/Build/Products/Debug-iphonesimulator"
RUNNER_APP="$PRODUCTS/TapwingRunnerUITests-Runner.app"
HOST_APP="$PRODUCTS/TapwingRunner.app"
[[ -d "$RUNNER_APP" ]] || die "xcodebuild did not produce $RUNNER_APP"
[[ -d "$HOST_APP" ]] || die "xcodebuild did not produce $HOST_APP"

XCTESTRUN_SRC=$(find "$DERIVED/Build/Products" -maxdepth 1 -name '*.xctestrun' | head -n 1)
[[ -f "$XCTESTRUN_SRC" ]] || die "xcodebuild did not produce an .xctestrun file"

# Strip non-essential debug symbol folders if any were emitted, keeping Mach-O executables intact.
find "$RUNNER_APP" "$HOST_APP" -name '*.dSYM' -exec rm -rf {} + 2>/dev/null || true

rm -rf "$OUT"
mkdir -p "$OUT/Debug-iphonesimulator"

# Copy unpacked bundles for immediate local dev / $TAPWING_IOS_DRIVER_DIR usage.
cp -R "$RUNNER_APP" "$OUT/Debug-iphonesimulator/TapwingRunnerUITests-Runner.app"
cp -R "$HOST_APP" "$OUT/Debug-iphonesimulator/TapwingRunner.app"

# Normalize the .xctestrun plist so it has no machine-specific paths, never times out,
# keeps zero attachments, and runs only RunnerTest/testHttpServer.
python3 - "$XCTESTRUN_SRC" "$OUT/tapwing-ios-runner.xctestrun" <<'PY'
import plistlib, sys

src, dst = sys.argv[1], sys.argv[2]
with open(src, "rb") as f:
    data = plistlib.load(f)

def patch_target(cfg):
    cfg["TestTimeoutsEnabled"] = False
    cfg["SystemAttachmentLifetime"] = "keepNever"
    cfg["UserAttachmentLifetime"] = "keepNever"
    cfg.pop("OnlyTestIdentifiers", None)
    cfg.pop("SkipTestIdentifiers", None)

# Support both FormatVersion 1 (top-level target dict) and FormatVersion 2 (TestConfigurations).
if "TestConfigurations" in data:
    for tc in data["TestConfigurations"]:
        for tgt in tc.get("TestTargets", []):
            patch_target(tgt)
elif "TapwingRunnerUITests" in data:
    patch_target(data["TapwingRunnerUITests"])
else:
    raise SystemExit(f"unexpected xctestrun structure: {list(data.keys())}")

with open(dst, "wb") as f:
    plistlib.dump(data, f, fmt=plistlib.FMT_XML, sort_keys=True)
PY

# Package zips matching the layout expected by `ios_native_lane::extract_driver_with`
# (symlinks preserved with -y).
(cd "$OUT/Debug-iphonesimulator" && zip -qry "$OUT/tapwing-ios-runnerUITests-Runner.zip" TapwingRunnerUITests-Runner.app)
(cd "$OUT/Debug-iphonesimulator" && zip -qry "$OUT/tapwing-ios-runner.zip" TapwingRunner.app)

# Record the toolchain that built the runner. `tapwing` compares it with the
# local Xcode and prints one warning on a major-version mismatch
# (`ios_native_lane/xcode_compat.rs`); the file is optional for it.
xcode_info=$(xcodebuild -version)
sdk_version=$(xcodebuild -version -sdk iphonesimulator SDKVersion 2>/dev/null || true)
sdk_build=$(xcodebuild -version -sdk iphonesimulator ProductBuildVersion 2>/dev/null || true)
python3 - "$OUT/tapwing-ios-runner.build-info.json" "$xcode_info" "$sdk_version" "$sdk_build" <<'PY'
import json, re, sys

dst, xcode_info, sdk_version, sdk_build = sys.argv[1:5]
version = re.search(r"^Xcode\s+(\S+)", xcode_info, re.M)
build = re.search(r"^Build version\s+(\S+)", xcode_info, re.M)
if not version:
    raise SystemExit(f"unexpected `xcodebuild -version` output: {xcode_info!r}")
info = {
    "xcodeVersion": version.group(1),
    "xcodeBuild": build.group(1) if build else "",
    "sdkVersion": sdk_version.strip(),
    "sdkBuild": sdk_build.strip(),
}
with open(dst, "w") as f:
    json.dump(info, f, indent=2, sort_keys=True)
    f.write("\n")
PY

for f in tapwing-ios-runner.xctestrun tapwing-ios-runnerUITests-Runner.zip tapwing-ios-runner.zip \
  tapwing-ios-runner.build-info.json; do
  sum=$(sha256 "$OUT/$f")
  printf '%s  %s\n' "$sum" "$f" >"$OUT/$f.sha256"
  echo "$OUT/$f  sha256 $sum"
done

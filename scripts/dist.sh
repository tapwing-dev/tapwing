#!/usr/bin/env bash
# Builds Tapwing release tarballs with Tapwing's own native-lane drivers bundled.
#
#   scripts/dist.sh                          # every installed target of the default list
#   scripts/dist.sh --target aarch64-apple-darwin
#   scripts/dist.sh --target x86_64-apple-darwin
#   scripts/dist.sh --target x86_64-unknown-linux-gnu
#   scripts/dist.sh --out-dir /tmp/dist      # override output dir (default: dist/)
#   scripts/dist.sh --no-android-driver      # without Tapwing's Android driver APK
#   scripts/dist.sh --no-ios-driver          # without Tapwing's iOS Simulator driver
# A tarball built with either flag has no native lane for that platform.
#
# Output: dist/tapwing-<version>-<target>.tar.gz + .sha256, laid out as
#   tapwing-<version>-<target>/
#     LICENSE                                    (Tapwing, Apache-2.0)
#     install.sh
#     bin/tapwing
#     share/tapwing/drivers/android/tapwing-driver.apk (+ .sha256; drivers/android, F11a)
#     share/tapwing/drivers/ios/tapwing-ios-runner.* (+ .sha256; drivers/ios, F11b, *-apple-darwin only;
#       includes tapwing-ios-runner.build-info.json when the driver directory has one)
#     share/tapwing/drivers/SHA256SUMS
#     share/tapwing/licenses/THIRD_PARTY_NOTICES.md
#     share/tapwing/release
#
# Tapwing's Android driver is built from source with scripts/build-android-driver.sh
# (JDK 17+ and Android SDK platform 37), or taken from $TAPWING_ANDROID_DRIVER_APK.
# Its iOS Simulator driver (*-apple-darwin only) is built from source with
# scripts/build-ios-driver.sh (Xcode), or taken from $TAPWING_IOS_DRIVER_DIR.
# Nothing is downloaded and nothing is uploaded.
#
# Env: TAPWING_ANDROID_DRIVER_APK=<path> ships a prebuilt tapwing-driver.apk instead of building.
# TAPWING_IOS_DRIVER_DIR=<dir> ships prebuilt tapwing-ios-runner.{xctestrun,zip} instead of building.
# TAPWING_PREBUILT_BIN=<path> packages an already-built (e.g. signed) tapwing binary for one --target.
# TAPWING_DIST_OUT_DIR=<dir> overrides the output directory (same as --out-dir).
set -euo pipefail

DEFAULT_TARGETS=(aarch64-apple-darwin x86_64-apple-darwin x86_64-unknown-linux-gnu)

ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
CRATE="$ROOT/crates/tapwing"
OUT="${TAPWING_DIST_OUT_DIR:-$ROOT/dist}"

die() { echo "dist: $*" >&2; exit 1; }
note() { echo "dist: $*" >&2; }

sha256() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | cut -d' ' -f1
  elif command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$1" | cut -d' ' -f1
  else
    die "sha256sum or shasum is required"
  fi
}

explicit_targets=()
with_android_driver=true
with_ios_driver=true
while [[ $# -gt 0 ]]; do
  case "$1" in
    --target) [[ $# -ge 2 ]] || die "--target needs a value"; explicit_targets+=("$2"); shift 2 ;;
    --out-dir) [[ $# -ge 2 ]] || die "--out-dir needs a value"; OUT="$2"; shift 2 ;;
    --no-android-driver) with_android_driver=false; shift ;;
    --no-ios-driver) with_ios_driver=false; shift ;;
    -h|--help) awk 'NR > 1 && /^#/ { sub(/^# ?/, ""); print; next } NR > 1 { exit }' "$0"; exit 0 ;;
    *) die "unknown argument: $1" ;;
  esac
done

for tool in cargo tar; do command -v "$tool" >/dev/null || die "$tool is required"; done
VERSION=$(sed -n 's/^version = "\(.*\)"$/\1/p' "$CRATE/Cargo.toml" | head -n1)
[[ -n "$VERSION" ]] || die "no version in $CRATE/Cargo.toml"

prebuilt_bin="${TAPWING_PREBUILT_BIN:-}"
if [[ -n "$prebuilt_bin" ]]; then
  [[ -f "$prebuilt_bin" && -x "$prebuilt_bin" ]] || die "TAPWING_PREBUILT_BIN=$prebuilt_bin does not exist or is not executable"
  [[ ${#explicit_targets[@]} -eq 1 ]] || die "TAPWING_PREBUILT_BIN requires exactly one --target"
  targets=("${explicit_targets[0]}")
else
  installed_targets=$(rustup target list --installed 2>/dev/null || rustc -vV | sed -n 's/^host: //p')
  targets=()
  if [[ ${#explicit_targets[@]} -gt 0 ]]; then
    for t in "${explicit_targets[@]}"; do
      grep -qx "$t" <<<"$installed_targets" || die "Rust target $t is not installed (rustup target add $t)"
      targets+=("$t")
    done
  else
    for t in "${DEFAULT_TARGETS[@]}"; do
      if grep -qx "$t" <<<"$installed_targets"; then targets+=("$t"); else note "skipping $t: Rust target not installed"; fi
    done
  fi
fi
[[ ${#targets[@]} -gt 0 ]] || die "no target to build"

# --- Drivers --------------------------------------------------------------------
WORK=$(mktemp -d "${TMPDIR:-/tmp}/tapwing-dist.XXXXXX")
trap 'rm -rf "$WORK"' EXIT

needs_ios=false
for t in "${targets[@]}"; do
  if [[ "$t" == *-apple-darwin ]]; then needs_ios=true; fi
done

# Tapwing's Android driver.
tapwing_apk=""
if $with_android_driver; then
  if [[ -n "${TAPWING_ANDROID_DRIVER_APK:-}" ]]; then
    tapwing_apk="$TAPWING_ANDROID_DRIVER_APK"
    [[ -f "$tapwing_apk" ]] || die "TAPWING_ANDROID_DRIVER_APK=$tapwing_apk does not exist"
  else
    "$ROOT/scripts/build-android-driver.sh" --no-test >&2 \
      || die "building the Tapwing Android driver failed (needs JDK 17+ and the Android SDK; or pass --no-android-driver)"
    tapwing_apk="$ROOT/drivers/android/build/tapwing-driver.apk"
  fi
  cp "$tapwing_apk" "$WORK/tapwing-driver.apk"
  TAPWING_APK_SHA256=$(sha256 "$WORK/tapwing-driver.apk")
  note "Tapwing Android driver sha256 $TAPWING_APK_SHA256"
fi

# Tapwing's iOS Simulator driver (macOS targets only).
TAPWING_IOS_FILES=(tapwing-ios-runner.xctestrun tapwing-ios-runnerUITests-Runner.zip tapwing-ios-runner.zip)
TAPWING_IOS_BUILD_INFO=tapwing-ios-runner.build-info.json
tapwing_ios_dir=""
if $needs_ios && $with_ios_driver; then
  if [[ -n "${TAPWING_IOS_DRIVER_DIR:-}" ]]; then
    tapwing_ios_dir="$TAPWING_IOS_DRIVER_DIR"
    [[ -d "$tapwing_ios_dir" ]] || die "TAPWING_IOS_DRIVER_DIR=$tapwing_ios_dir does not exist"
  else
    "$ROOT/scripts/build-ios-driver.sh" --no-test >&2 \
      || die "building the Tapwing iOS driver failed (needs macOS + Xcode; or pass --no-ios-driver)"
    tapwing_ios_dir="$ROOT/drivers/ios/build"
  fi
  mkdir -p "$WORK/tapwing-ios"
  for f in "${TAPWING_IOS_FILES[@]}"; do
    [[ -f "$tapwing_ios_dir/$f" ]] || die "Tapwing iOS driver missing $tapwing_ios_dir/$f"
    cp "$tapwing_ios_dir/$f" "$WORK/tapwing-ios/$f"
    sum=$(sha256 "$WORK/tapwing-ios/$f")
    printf '%s  %s\n' "$sum" "$f" >"$WORK/tapwing-ios/$f.sha256"
    note "Tapwing iOS driver $f sha256 $sum"
  done
  # Build Xcode / SDK of the runner: shipped when present (a driver directory
  # built before the file existed has none, and `tapwing` works without it).
  if [[ -f "$tapwing_ios_dir/$TAPWING_IOS_BUILD_INFO" ]]; then
    cp "$tapwing_ios_dir/$TAPWING_IOS_BUILD_INFO" "$WORK/tapwing-ios/$TAPWING_IOS_BUILD_INFO"
    sum=$(sha256 "$WORK/tapwing-ios/$TAPWING_IOS_BUILD_INFO")
    printf '%s  %s\n' "$sum" "$TAPWING_IOS_BUILD_INFO" >"$WORK/tapwing-ios/$TAPWING_IOS_BUILD_INFO.sha256"
    TAPWING_IOS_FILES+=("$TAPWING_IOS_BUILD_INFO")
  fi
fi

# --- Per-target tarballs ----------------------------------------------------------
if tar --version 2>/dev/null | grep -q bsdtar; then
  tar_owner=(--uid 0 --gid 0 --uname root --gname root)
else
  tar_owner=(--owner=0 --group=0 --numeric-owner)
fi
host=$(rustc -vV | sed -n 's/^host: //p')
built=()

for target in "${targets[@]}"; do
  if [[ -n "$prebuilt_bin" ]]; then
    note "packaging prebuilt binary for $target"
    bin_path="$prebuilt_bin"
  else
    note "building $target"
    if ! (cd "$CRATE" && cargo build --release --locked --target "$target"); then
      if [[ ${#explicit_targets[@]} -gt 0 ]]; then
        die "failed to build $target (a cross target needs its linker and C compiler, see docs/install.md)"
      fi
      note "FAILED to build $target (a cross target needs its linker and C compiler, see docs/install.md)"
      continue
    fi
    bin_path="$CRATE/target/$target/release/tapwing"
  fi
  name="tapwing-$VERSION-$target"
  stage="$WORK/stage/$name"
  share="$stage/share/tapwing"
  mkdir -p "$stage/bin" "$share/drivers" "$share/licenses"
  install -m 0755 "$bin_path" "$stage/bin/tapwing"
  install -m 0755 "$ROOT/scripts/install.sh" "$stage/install.sh"
  install -m 0644 "$ROOT/LICENSE" "$stage/LICENSE"
  : >"$share/drivers/SHA256SUMS"
  if [[ -n "$tapwing_apk" ]]; then
    mkdir -p "$share/drivers/android"
    cp "$WORK/tapwing-driver.apk" "$share/drivers/android/tapwing-driver.apk"
    printf '%s  %s\n' "$TAPWING_APK_SHA256" tapwing-driver.apk >"$share/drivers/android/tapwing-driver.apk.sha256"
    printf '%s  %s\n' "$TAPWING_APK_SHA256" android/tapwing-driver.apk >>"$share/drivers/SHA256SUMS"
  fi
  target_has_tapwing_ios=false
  if [[ "$target" == *-apple-darwin && -n "$tapwing_ios_dir" ]]; then
    target_has_tapwing_ios=true
    mkdir -p "$share/drivers/ios"
    for f in "${TAPWING_IOS_FILES[@]}"; do
      cp "$WORK/tapwing-ios/$f" "$share/drivers/ios/$f"
      cp "$WORK/tapwing-ios/$f.sha256" "$share/drivers/ios/$f.sha256"
      sum=$(cut -d' ' -f1 "$WORK/tapwing-ios/$f.sha256")
      printf '%s  %s\n' "$sum" "ios/$f" >>"$share/drivers/SHA256SUMS"
    done
  fi
  chmod -R u=rwX,go=rX "$share/drivers"
  printf 'version=%s\ntarget=%s\n' "$VERSION" "$target" >"$share/release"
  {
    cat <<EOF
# Third-party notices

Tapwing $VERSION ($target) is licensed under the Apache License 2.0 (\`LICENSE\` at the top of this
release) and bundles the third-party software below.

SHA-256 of the bundled drivers: \`drivers/SHA256SUMS\`. Parts of both drivers are ported from
Maestro (Apache License 2.0); their notices follow.
EOF
    if [[ -n "$tapwing_apk" ]]; then
      cat <<EOF

## Tapwing Android driver (\`drivers/android/tapwing-driver.apk\`)

Built from this repository's \`drivers/android\` (SHA-256 \`$TAPWING_APK_SHA256\`). Its notices follow.

EOF
      sed 's/^#/##/' "$ROOT/drivers/android/THIRD_PARTY_NOTICES.md" | tail -n +2
    fi
    if $target_has_tapwing_ios; then
      cat <<EOF

## Tapwing iOS Simulator driver (\`drivers/ios/tapwing-ios-runner.*\`)

Built from this repository's \`drivers/ios\`. Its notices follow.

EOF
      sed 's/^#/##/' "$ROOT/drivers/ios/THIRD_PARTY_NOTICES.md" | tail -n +2
    fi
    cat <<EOF

## Rust crates linked into \`bin/tapwing\`

Name, version and declared license of every crate in the normal dependency graph for $target
(from \`cargo tree\`). Their license texts are not reproduced here yet.

EOF
    (cd "$CRATE" && cargo tree --locked --target "$target" -e normal --prefix none --format '{p} | {l}' 2>/dev/null) \
      | sed 's/ (\/[^)]*)//; s/ (\*)$//' | grep -v -e '^tapwing v' -e '(proc-macro)' | sort -u | sed 's/^/- /'
  } >"$share/licenses/THIRD_PARTY_NOTICES.md"

  if [[ "$target" == "$host" ]]; then
    # The staged binary must find and accept its own bundle, with every override unset.
    report=$(env -u TAPWING_DRIVER_APK -u TAPWING_IOS_DRIVER_DIR "$stage/bin/tapwing" --version) \
      || die "staged tapwing --version failed: $report"
    if [[ -n "$tapwing_apk" ]]; then
      grep -q "tapwing android driver: $VERSION (bundled)" <<<"$report" || die "staged binary does not use its bundled Tapwing driver: $report"
    fi
    if $target_has_tapwing_ios; then
      grep -q "tapwing ios driver: $VERSION (bundled)" <<<"$report" || die "staged binary does not use its bundled Tapwing iOS driver: $report"
    elif [[ "$target" != *-apple-darwin ]]; then
      grep -q "tapwing ios driver: n/a" <<<"$report" || die "staged non-macOS binary should report iOS driver as n/a: $report"
    fi
  fi

  tarball="$OUT/$name.tar.gz"
  mkdir -p "$OUT"
  COPYFILE_DISABLE=1 tar "${tar_owner[@]}" -C "$WORK/stage" -czf "$tarball.part" "$name"
  mv "$tarball.part" "$tarball"
  (cd "$OUT" && printf '%s  %s\n' "$(sha256 "$name.tar.gz")" "$name.tar.gz" >"$name.tar.gz.sha256")
  built+=("$tarball")
done

[[ ${#built[@]} -gt 0 ]] || die "no tarball was built"
for t in "${built[@]}"; do
  note "$(du -h "$t" | cut -f1 | tr -d ' ')  $t"
done

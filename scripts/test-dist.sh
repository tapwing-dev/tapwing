#!/usr/bin/env bash
# Offline tests for scripts/dist.sh, scripts/install.sh, scripts/generate-homebrew-formula.sh
# and scripts/release.sh (in a scratch repository with a local bare origin).
# Exercises tarball layout, checksums, Linux vs macOS driver bundling,
# sha256sum/shasum fallback, uname OS/arch detection, install/upgrade/uninstall,
# --version / --version latest / piped `curl | sh` installs (through a curl stand-in),
# the Homebrew formula, and syntax checks without needing a device, cross-compiler, or network.
set -euo pipefail

ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
VERSION=$(sed -n 's/^version = "\(.*\)"$/\1/p' "$ROOT/crates/tapwing/Cargo.toml" | head -n1)
[[ -n "$VERSION" ]] || { echo "FAIL: could not read version from Cargo.toml" >&2; exit 1; }

WORK=$(mktemp -d "${TMPDIR:-/tmp}/tapwing-dist-test.XXXXXX")
trap 'rm -rf "$WORK"' EXIT

pass() { echo "PASS: $*"; }
fail() { echo "FAIL: $*" >&2; exit 1; }

sha256_file() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | cut -d' ' -f1
  else
    shasum -a 256 "$1" | cut -d' ' -f1
  fi
}

# 1. Shell syntax checks.
bash -n "$ROOT/scripts/dist.sh" \
        "$ROOT/scripts/build-android-driver.sh" \
        "$ROOT/scripts/build-ios-driver.sh" \
        "$ROOT/scripts/generate-homebrew-formula.sh" \
        "$ROOT/scripts/release.sh" \
        "$ROOT/scripts/test-dist.sh"
sh -n "$ROOT/scripts/install.sh"
pass "shell syntax checks (bash -n, sh -n)"

# 2. Fake driver artifacts + fake tapwing binaries.
FAKE_APK="$WORK/tapwing-driver.apk"
printf 'fake-android-driver-apk-content\n' >"$FAKE_APK"

FAKE_IOS="$WORK/ios-driver"
mkdir -p "$FAKE_IOS"
printf '<plist>xctestrun</plist>\n' >"$FAKE_IOS/tapwing-ios-runner.xctestrun"
printf 'fake-uitests-zip\n' >"$FAKE_IOS/tapwing-ios-runnerUITests-Runner.zip"
printf 'fake-host-zip\n' >"$FAKE_IOS/tapwing-ios-runner.zip"
printf '{"xcodeVersion":"26.1","xcodeBuild":"17B55","sdkVersion":"26.1","sdkBuild":"23B77"}\n' \
  >"$FAKE_IOS/tapwing-ios-runner.build-info.json"
# A driver directory built before build-info existed: still packaged, without the file.
FAKE_IOS_OLD="$WORK/ios-driver-old"
mkdir -p "$FAKE_IOS_OLD"
cp "$FAKE_IOS/tapwing-ios-runner.xctestrun" "$FAKE_IOS/tapwing-ios-runnerUITests-Runner.zip" \
  "$FAKE_IOS/tapwing-ios-runner.zip" "$FAKE_IOS_OLD/"

make_fake_bin() {
  local path=$1 ios_line=$2
  cat >"$path" <<EOF
#!/bin/sh
if [ "\${1:-}" = "--version" ]; then
  printf 'tapwing %s\n' "$VERSION"
  printf 'tapwing android driver: %s (bundled) /fake/android/tapwing-driver.apk\n' "$VERSION"
  printf '%s\n' "$ios_line"
  exit 0
fi
exit 1
EOF
  chmod +x "$path"
}

FAKE_MAC_BIN="$WORK/tapwing-macos"
make_fake_bin "$FAKE_MAC_BIN" "tapwing ios driver: $VERSION (bundled) /fake/ios"

FAKE_LINUX_BIN="$WORK/tapwing-linux"
make_fake_bin "$FAKE_LINUX_BIN" "tapwing ios driver: n/a (the iOS Simulator needs macOS)"

OUT="$WORK/dist"

# Helper: verify tarball checksum + inner SHA256SUMS + exact entry list.
# expect_ios: yes (with build-info) | old (without build-info) | no.
verify_tarball() {
  local target=$1 expect_ios=$2
  local expect_build_info=no
  case "$expect_ios" in
    yes) expect_build_info=yes ;;
    old) expect_ios=yes ;;
  esac
  local name="tapwing-$VERSION-$target"
  local tb="$OUT/$name.tar.gz"
  local sumfile="$tb.sha256"

  [[ -f "$tb" ]] || fail "missing tarball $tb"
  [[ -f "$sumfile" ]] || fail "missing checksum $sumfile"

  local got want
  got=$(sha256_file "$tb")
  want=$(cut -d' ' -f1 <"$sumfile")
  [[ "$got" == "$want" ]] || fail "$tb checksum mismatch: got $got, expected $want"
  [[ "$(cat "$sumfile")" == "$want  $name.tar.gz" ]] || fail "$sumfile format mismatch"

  local unpack="$WORK/unpack-$target"
  mkdir -p "$unpack"
  tar -xzf "$tb" -C "$unpack"

  # Check exact file list inside tarball (excluding directory entries).
  local actual_files expected_files
  actual_files=$(cd "$unpack" && find "$name" -type f | LC_ALL=C sort)
  if [[ "$expect_ios" == "yes" ]]; then
    expected_files=$(cat <<EOF | LC_ALL=C sort
$name/LICENSE
$name/bin/tapwing
$name/install.sh
$name/share/tapwing/drivers/SHA256SUMS
$name/share/tapwing/drivers/android/tapwing-driver.apk
$name/share/tapwing/drivers/android/tapwing-driver.apk.sha256
$name/share/tapwing/drivers/ios/tapwing-ios-runner.xctestrun
$name/share/tapwing/drivers/ios/tapwing-ios-runner.xctestrun.sha256
$name/share/tapwing/drivers/ios/tapwing-ios-runner.zip
$name/share/tapwing/drivers/ios/tapwing-ios-runner.zip.sha256
$name/share/tapwing/drivers/ios/tapwing-ios-runnerUITests-Runner.zip
$name/share/tapwing/drivers/ios/tapwing-ios-runnerUITests-Runner.zip.sha256
$name/share/tapwing/licenses/THIRD_PARTY_NOTICES.md
$name/share/tapwing/release
EOF
)
    if [[ "$expect_build_info" == "yes" ]]; then
      expected_files=$(printf '%s\n%s\n%s\n' "$expected_files" \
        "$name/share/tapwing/drivers/ios/tapwing-ios-runner.build-info.json" \
        "$name/share/tapwing/drivers/ios/tapwing-ios-runner.build-info.json.sha256" | LC_ALL=C sort)
    fi
  else
    expected_files=$(cat <<EOF | LC_ALL=C sort
$name/LICENSE
$name/bin/tapwing
$name/install.sh
$name/share/tapwing/drivers/SHA256SUMS
$name/share/tapwing/drivers/android/tapwing-driver.apk
$name/share/tapwing/drivers/android/tapwing-driver.apk.sha256
$name/share/tapwing/licenses/THIRD_PARTY_NOTICES.md
$name/share/tapwing/release
EOF
)
    [[ ! -e "$unpack/$name/share/tapwing/drivers/ios" ]] || fail "$target must not contain drivers/ios"
  fi

  if [[ "$actual_files" != "$expected_files" ]]; then
    diff -u <(printf '%s\n' "$expected_files") <(printf '%s\n' "$actual_files") >&2 || true
    fail "$target tarball layout mismatch"
  fi

  # Verify release metadata.
  local rel="$unpack/$name/share/tapwing/release"
  grep -qx "version=$VERSION" "$rel" || fail "$target release missing version=$VERSION"
  grep -qx "target=$target" "$rel" || fail "$target release missing target=$target"

  # Verify every entry in share/tapwing/drivers/SHA256SUMS and sidecars.
  local droot="$unpack/$name/share/tapwing/drivers"
  local listed expected_listed="android/tapwing-driver.apk"
  if [[ "$expect_ios" == "yes" ]]; then
    expected_listed+=$'\nios/tapwing-ios-runner.xctestrun\nios/tapwing-ios-runner.zip\nios/tapwing-ios-runnerUITests-Runner.zip'
  fi
  if [[ "$expect_build_info" == "yes" ]]; then
    expected_listed+=$'\nios/tapwing-ios-runner.build-info.json'
    grep -q '"xcodeVersion":"26.1"' "$droot/ios/tapwing-ios-runner.build-info.json" \
      || fail "$target build-info.json was not copied verbatim"
  fi
  listed=$(awk '{ print $2 }' "$droot/SHA256SUMS" | LC_ALL=C sort)
  [[ "$listed" == "$(LC_ALL=C sort <<<"$expected_listed")" ]] \
    || fail "$target SHA256SUMS lists [$listed], expected [$expected_listed]"
  while read -r hash relpath; do
    [[ -n "$hash" ]] || continue
    [[ -f "$droot/$relpath" ]] || fail "$target missing $relpath listed in SHA256SUMS"
    local actual_h sidecar_h
    actual_h=$(sha256_file "$droot/$relpath")
    [[ "$actual_h" == "$hash" ]] || fail "$target $relpath hash mismatch in SHA256SUMS"
    sidecar_h=$(cut -d' ' -f1 <"$droot/$relpath.sha256")
    [[ "$sidecar_h" == "$hash" ]] || fail "$target $relpath.sha256 mismatch"
  done <"$droot/SHA256SUMS"

  # Verify THIRD_PARTY_NOTICES.md.
  local notices="$unpack/$name/share/tapwing/licenses/THIRD_PARTY_NOTICES.md"
  grep -q "Tapwing $VERSION ($target)" "$notices" || fail "$target notices missing header"
  grep -q "Tapwing Android driver" "$notices" || fail "$target notices missing Android driver"
  if [[ "$expect_ios" == "yes" ]]; then
    grep -q "Tapwing iOS Simulator driver" "$notices" || fail "$target notices missing iOS driver"
  else
    if grep -q "Tapwing iOS Simulator driver" "$notices"; then
      fail "$target notices must not mention iOS driver"
    fi
  fi
}

# 3. Package all three targets with dist.sh.
TAPWING_PREBUILT_BIN="$FAKE_MAC_BIN" \
TAPWING_ANDROID_DRIVER_APK="$FAKE_APK" \
TAPWING_IOS_DRIVER_DIR="$FAKE_IOS" \
  "$ROOT/scripts/dist.sh" --target aarch64-apple-darwin --out-dir "$OUT"
verify_tarball aarch64-apple-darwin yes
pass "dist.sh layout & checksums for aarch64-apple-darwin"

TAPWING_PREBUILT_BIN="$FAKE_MAC_BIN" \
TAPWING_ANDROID_DRIVER_APK="$FAKE_APK" \
TAPWING_IOS_DRIVER_DIR="$FAKE_IOS_OLD" \
  "$ROOT/scripts/dist.sh" --target x86_64-apple-darwin --out-dir "$OUT"
verify_tarball x86_64-apple-darwin old
pass "dist.sh layout & checksums for x86_64-apple-darwin (iOS driver without build-info)"

# Linux target must omit iOS driver even if TAPWING_IOS_DRIVER_DIR is set.
TAPWING_PREBUILT_BIN="$FAKE_LINUX_BIN" \
TAPWING_ANDROID_DRIVER_APK="$FAKE_APK" \
TAPWING_IOS_DRIVER_DIR="$FAKE_IOS" \
  "$ROOT/scripts/dist.sh" --target x86_64-unknown-linux-gnu --out-dir "$OUT"
verify_tarball x86_64-unknown-linux-gnu no
pass "dist.sh layout & checksums for x86_64-unknown-linux-gnu (Android driver only)"

# 3b. Homebrew formula from the three tarballs' .sha256 files.
FORMULA="$WORK/Formula/tapwing.rb"
"$ROOT/scripts/generate-homebrew-formula.sh" --version "v$VERSION" --dist-dir "$OUT" --repo o/r --out "$FORMULA"
for target in aarch64-apple-darwin x86_64-apple-darwin x86_64-unknown-linux-gnu; do
  name="tapwing-$VERSION-$target.tar.gz"
  grep -qF "url \"https://github.com/o/r/releases/download/v$VERSION/$name\"" "$FORMULA" || fail "formula lacks the $target URL"
  grep -qF "sha256 \"$(cut -d' ' -f1 <"$OUT/$name.sha256")\"" "$FORMULA" || fail "formula lacks the $target sha256"
done
grep -qx "  version \"$VERSION\"" "$FORMULA" || fail "formula lacks version $VERSION"
grep -qF 'homepage "https://github.com/o/r"' "$FORMULA" || fail "formula lacks the homepage"
grep -qF '(share/"tapwing").install Dir["share/tapwing/*"]' "$FORMULA" || fail "formula does not install share/tapwing"
[[ "$("$ROOT/scripts/generate-homebrew-formula.sh" --version "$VERSION" --dist-dir "$OUT" --repo o/r)" == "$(cat "$FORMULA")" ]] \
  || fail "formula on stdout differs from --out"
if command -v ruby >/dev/null 2>&1; then
  ruby -c "$FORMULA" >/dev/null || fail "formula is not valid Ruby"
else
  echo "NOTE: ruby not found; skipping ruby -c of the formula"
fi
mkdir -p "$WORK/dist-partial"
cp "$OUT"/*.sha256 "$WORK/dist-partial/"
rm "$WORK/dist-partial/tapwing-$VERSION-x86_64-apple-darwin.tar.gz.sha256"
if "$ROOT/scripts/generate-homebrew-formula.sh" --version "$VERSION" --dist-dir "$WORK/dist-partial" >/dev/null 2>&1; then
  fail "generate-homebrew-formula.sh accepted a dist dir without the x86_64-apple-darwin .sha256"
fi
cp "$OUT/tapwing-$VERSION-x86_64-apple-darwin.tar.gz.sha256" "$WORK/dist-partial/"
printf 'NOT-A-HASH  tapwing-%s-x86_64-apple-darwin.tar.gz\n' "$VERSION" >"$WORK/dist-partial/tapwing-$VERSION-x86_64-apple-darwin.tar.gz.sha256"
if "$ROOT/scripts/generate-homebrew-formula.sh" --version "$VERSION" --dist-dir "$WORK/dist-partial" >/dev/null 2>&1; then
  fail "generate-homebrew-formula.sh accepted a malformed .sha256"
fi
pass "generate-homebrew-formula.sh emits all three URLs + hashes and rejects a missing or malformed .sha256"

# 4. Verify sha256sum fallback when shasum is unavailable (as on minimal Linux).
if command -v sha256sum >/dev/null 2>&1; then
  NOSHASUM="$WORK/bin-noshasum"
  mkdir -p "$NOSHASUM"
  cat >"$NOSHASUM/shasum" <<'EOF'
#!/bin/sh
echo "shasum should not be called when sha256sum exists" >&2
exit 99
EOF
  chmod +x "$NOSHASUM/shasum"
  PATH="$NOSHASUM:$PATH" \
  TAPWING_PREBUILT_BIN="$FAKE_LINUX_BIN" \
  TAPWING_ANDROID_DRIVER_APK="$FAKE_APK" \
    "$ROOT/scripts/dist.sh" --target x86_64-unknown-linux-gnu --out-dir "$WORK/dist-sha256sum"
  [[ -f "$WORK/dist-sha256sum/tapwing-$VERSION-x86_64-unknown-linux-gnu.tar.gz.sha256" ]] \
    || fail "dist.sh failed with sha256sum"
  pass "dist.sh prefers sha256sum over shasum"
fi

# 5. Test install.sh OS/arch detection via uname (and Rosetta detection on macOS).
check_uname_target() {
  local os=$1 arch=$2 expect=$3 arm64_sysctl=${4:-0}
  local mockdir="$WORK/uname-$os-$arch-$arm64_sysctl"
  mkdir -p "$mockdir"
  cat >"$mockdir/uname" <<EOF
#!/bin/sh
case "\${1:-}" in
  -s) echo "$os" ;;
  -m) echo "$arch" ;;
  *) echo "$os $arch" ;;
esac
EOF
  cat >"$mockdir/sysctl" <<EOF
#!/bin/sh
if [ "\${1:-}" = "-n" ] && [ "\${2:-}" = "hw.optional.arm64" ]; then
  echo "$arm64_sysctl"
  exit 0
fi
exit 1
EOF
  chmod +x "$mockdir/uname" "$mockdir/sysctl"
  local got
  if [[ "$expect" == "FAIL" ]]; then
    if PATH="$mockdir:$PATH" "$ROOT/scripts/install.sh" --print-target >/dev/null 2>&1; then
      fail "install.sh --print-target should have failed for $os $arch"
    fi
  else
    got=$(PATH="$mockdir:$PATH" "$ROOT/scripts/install.sh" --print-target)
    [[ "$got" == "$expect" ]] || fail "uname $os/$arch (arm64=$arm64_sysctl): got '$got', expected '$expect'"
  fi
}

check_uname_target Darwin arm64 aarch64-apple-darwin 1
check_uname_target Darwin x86_64 x86_64-apple-darwin 0
check_uname_target Darwin x86_64 aarch64-apple-darwin 1
check_uname_target Linux x86_64 x86_64-unknown-linux-gnu 0
check_uname_target Linux aarch64 aarch64-unknown-linux-gnu 0
check_uname_target FreeBSD x86_64 FAIL 0
pass "install.sh --print-target uname OS/arch and macOS Rosetta detection"

# 6. Test install.sh install, bad checksum rejection, wrong-platform rejection, upgrade, PATH rc, and uninstall.
# Isolate HOME so shell rc edits never touch the developer's real dotfiles.
TEST_HOME="$WORK/home"
mkdir -p "$TEST_HOME"
export HOME="$TEST_HOME"
unset ZDOTDIR XDG_CONFIG_HOME TAPWING_PREFIX TAPWING_BIN_DIR TAPWING_VERSION TAPWING_NO_MODIFY_PATH || true

HOST_TARGET=$("$ROOT/scripts/install.sh" --print-target)
case "$HOST_TARGET" in
  aarch64-apple-darwin|x86_64-apple-darwin|x86_64-unknown-linux-gnu)
    HOST_TB="$OUT/tapwing-$VERSION-$HOST_TARGET.tar.gz"
    if [[ "$HOST_TARGET" == *-apple-darwin ]]; then
      WRONG_TB="$OUT/tapwing-$VERSION-x86_64-unknown-linux-gnu.tar.gz"
    else
      WRONG_TB="$OUT/tapwing-$VERSION-aarch64-apple-darwin.tar.gz"
    fi

    PREFIX="$WORK/install-prefix"
    BINDIR="$WORK/install-bin"

    # Bad checksum fails.
    if "$ROOT/scripts/install.sh" "$HOST_TB" --prefix "$PREFIX" --bin-dir "$BINDIR" --sha256 0000000000000000000000000000000000000000000000000000000000000000 >/dev/null 2>&1; then
      fail "install.sh accepted bad --sha256"
    fi
    pass "install.sh rejects bad --sha256"

    # Wrong platform fails.
    if "$ROOT/scripts/install.sh" "$WRONG_TB" --prefix "$PREFIX" --bin-dir "$BINDIR" >/dev/null 2>&1; then
      fail "install.sh accepted wrong-platform tarball $WRONG_TB"
    fi
    pass "install.sh rejects wrong-platform tarball"

    # A corrupted .sha256 sidecar next to the tarball fails.
    mkdir -p "$WORK/corrupt"
    cp "$HOST_TB" "$WORK/corrupt/"
    printf '%s  %s\n' 0000000000000000000000000000000000000000000000000000000000000000 "$(basename "$HOST_TB")" \
      >"$WORK/corrupt/$(basename "$HOST_TB").sha256"
    if "$ROOT/scripts/install.sh" "$WORK/corrupt/$(basename "$HOST_TB")" --prefix "$PREFIX" --bin-dir "$BINDIR" >/dev/null 2>&1; then
      fail "install.sh accepted a tarball whose .sha256 sidecar does not match"
    fi
    [[ ! -e "$PREFIX/versions/$VERSION" ]] || fail "install.sh installed despite a checksum mismatch"
    pass "install.sh rejects a mismatching .sha256 sidecar"

    # Good install (auto-verifies .sha256 sidecar and runs self-check).
    install_log=$("$ROOT/scripts/install.sh" "$HOST_TB" --prefix "$PREFIX" --bin-dir "$BINDIR" 2>&1)
    grep -qx "tapwing $VERSION" <<<"$install_log" || fail "install.sh did not run the self-check: $install_log"
    [[ -L "$BINDIR/tapwing" ]] || fail "missing $BINDIR/tapwing symlink"
    [[ -L "$PREFIX/current" ]] || fail "missing $PREFIX/current symlink"
    [[ -x "$PREFIX/versions/$VERSION/bin/tapwing" ]] || fail "missing installed binary"
    pass "install.sh installs $HOST_TARGET tarball and passes self-check"

    # Simulated older version is pruned on re-install / upgrade.
    mkdir -p "$PREFIX/versions/0.0.1"
    "$ROOT/scripts/install.sh" "$HOST_TB" --prefix "$PREFIX" --bin-dir "$BINDIR"
    [[ ! -e "$PREFIX/versions/0.0.1" ]] || fail "install.sh did not prune old version 0.0.1"
    pass "install.sh prunes older versions on upgrade"

    # --version downloads https://github.com/<repo>/releases/download/v<ver>/<tarball> and
    # requires its .sha256 (offline, through a curl stand-in that serves $WORK/release).
    FAKE_CURL="$WORK/bin-fakecurl"
    mkdir -p "$FAKE_CURL" "$WORK/release"
    cat >"$FAKE_CURL/curl" <<'CURL'
#!/bin/sh
# Serves $TAPWING_TEST_RELEASE_DIR; resolves …/releases/latest to $TAPWING_TEST_LATEST_TAG
# (or to the releases page, as GitHub does when nothing is published).
out="" url="" write_out=""
while [ $# -gt 0 ]; do
  case "$1" in
    -o) out=$2; shift 2 ;;
    -w) write_out=$2; shift 2 ;;
    --retry) shift 2 ;;
    -*) shift ;;
    *) url=$1; shift ;;
  esac
done
echo "$url" >>"$TAPWING_TEST_CURL_LOG"
case "$url" in
  */releases/latest)
    [ "$write_out" = '%{url_effective}' ] || exit 2
    if [ -n "${TAPWING_TEST_LATEST_TAG:-}" ]; then
      printf '%s' "${url%/latest}/tag/$TAPWING_TEST_LATEST_TAG"
    else
      printf '%s' "${url%/latest}"
    fi
    exit 0
    ;;
esac
src="$TAPWING_TEST_RELEASE_DIR/$(basename "$url")"
[ -f "$src" ] || exit 22
cp "$src" "$out"
CURL
    chmod +x "$FAKE_CURL/curl"
    curl_env=(PATH="$FAKE_CURL:$PATH" TAPWING_TEST_CURL_LOG="$WORK/curl.log" TAPWING_TEST_RELEASE_DIR="$WORK/release")
    cp "$HOST_TB" "$WORK/release/"
    if env "${curl_env[@]}" "$ROOT/scripts/install.sh" --version "$VERSION" --repo o/r --prefix "$PREFIX" --bin-dir "$BINDIR" >/dev/null 2>&1; then
      fail "install.sh --version installed a release without its .sha256"
    fi
    pass "install.sh --version refuses a release without .sha256"
    cp "$HOST_TB.sha256" "$WORK/release/"
    : >"$WORK/curl.log"
    env "${curl_env[@]}" "$ROOT/scripts/install.sh" --version "v$VERSION" --repo o/r --prefix "$PREFIX" --bin-dir "$BINDIR" >/dev/null 2>&1 \
      || fail "install.sh --version failed"
    [[ "$(head -n1 "$WORK/curl.log")" == "https://github.com/o/r/releases/download/v$VERSION/tapwing-$VERSION-$HOST_TARGET.tar.gz" ]] \
      || fail "install.sh --version fetched $(head -n1 "$WORK/curl.log")"
    pass "install.sh --version downloads and verifies the GitHub Release tarball"

    # --version latest resolves the tag from the github.com/<repo>/releases/latest redirect.
    : >"$WORK/curl.log"
    rm -rf "$PREFIX/versions/$VERSION"
    env "${curl_env[@]}" TAPWING_TEST_LATEST_TAG="v$VERSION" \
      "$ROOT/scripts/install.sh" --version latest --repo o/r --prefix "$PREFIX" --bin-dir "$BINDIR" >/dev/null 2>&1 \
      || fail "install.sh --version latest failed"
    expected_log=$(printf '%s\n' \
      "https://github.com/o/r/releases/latest" \
      "https://github.com/o/r/releases/download/v$VERSION/tapwing-$VERSION-$HOST_TARGET.tar.gz" \
      "https://github.com/o/r/releases/download/v$VERSION/tapwing-$VERSION-$HOST_TARGET.tar.gz.sha256")
    [[ "$(cat "$WORK/curl.log")" == "$expected_log" ]] || fail "install.sh --version latest fetched: $(cat "$WORK/curl.log")"
    [[ -x "$PREFIX/versions/$VERSION/bin/tapwing" ]] || fail "install.sh --version latest did not install $VERSION"
    pass "install.sh --version latest resolves the redirect, downloads and verifies the release"

    # No release published yet: a clear error, nothing downloaded.
    : >"$WORK/curl.log"
    if latest_err=$(env "${curl_env[@]}" TAPWING_TEST_LATEST_TAG="" \
      "$ROOT/scripts/install.sh" --version latest --repo o/r --prefix "$PREFIX" --bin-dir "$BINDIR" 2>&1); then
      fail "install.sh --version latest succeeded without a published release"
    fi
    grep -q "no published GitHub Release" <<<"$latest_err" || fail "unclear error without a release: $latest_err"
    [[ "$(cat "$WORK/curl.log")" == "https://github.com/o/r/releases/latest" ]] || fail "install.sh downloaded without a release"
    pass "install.sh --version latest fails clearly when no release is published"

    # Latest still needs its .sha256.
    mv "$WORK/release/$(basename "$HOST_TB").sha256" "$WORK/release-sha256.bak"
    if env "${curl_env[@]}" TAPWING_TEST_LATEST_TAG="v$VERSION" \
      "$ROOT/scripts/install.sh" --prefix "$PREFIX" --bin-dir "$BINDIR" --repo o/r --version latest >/dev/null 2>&1; then
      fail "install.sh --version latest installed a release without its .sha256"
    fi
    mv "$WORK/release-sha256.bak" "$WORK/release/$(basename "$HOST_TB").sha256"
    pass "install.sh --version latest refuses a release without .sha256"

    # Piped into sh ($0 is the shell) with no SOURCE or --version: installs the latest release.
    : >"$WORK/curl.log"
    rm -rf "$PREFIX/versions/$VERSION"
    (cd "$WORK" && env "${curl_env[@]}" TAPWING_TEST_LATEST_TAG="v$VERSION" TAPWING_REPO=o/r \
      sh -s -- --prefix "$PREFIX" --bin-dir "$BINDIR" <"$ROOT/scripts/install.sh" >/dev/null 2>&1) \
      || fail "sh < install.sh failed"
    [[ "$(cat "$WORK/curl.log")" == "$expected_log" ]] || fail "sh < install.sh fetched: $(cat "$WORK/curl.log")"
    [[ -x "$PREFIX/versions/$VERSION/bin/tapwing" ]] || fail "sh < install.sh did not install $VERSION"
    pass "piped sh < install.sh installs the latest GitHub Release, checksum-verified"

    # Piped into bash (Grok-style `curl | bash` and `curl | bash -s <version>` + env vars).
    : >"$WORK/curl.log"
    rm -rf "$PREFIX/versions/$VERSION"
    (cd "$WORK" && env "${curl_env[@]}" TAPWING_TEST_LATEST_TAG="v$VERSION" TAPWING_REPO=o/r \
      TAPWING_PREFIX="$PREFIX" TAPWING_BIN_DIR="$BINDIR" TAPWING_NO_MODIFY_PATH=1 \
      bash <"$ROOT/scripts/install.sh" >/dev/null 2>&1) \
      || fail "bash < install.sh failed"
    [[ "$(cat "$WORK/curl.log")" == "$expected_log" ]] || fail "bash < install.sh fetched: $(cat "$WORK/curl.log")"
    [[ -x "$PREFIX/versions/$VERSION/bin/tapwing" ]] || fail "bash < install.sh did not install $VERSION"

    : >"$WORK/curl.log"
    rm -rf "$PREFIX/versions/$VERSION"
    (cd "$WORK" && env "${curl_env[@]}" TAPWING_REPO=o/r \
      TAPWING_PREFIX="$PREFIX" TAPWING_BIN_DIR="$BINDIR" TAPWING_NO_MODIFY_PATH=1 \
      bash -s "$VERSION" <"$ROOT/scripts/install.sh" >/dev/null 2>&1) \
      || fail "bash -s $VERSION < install.sh failed"
    expected_ver_log=$(printf '%s\n' \
      "https://github.com/o/r/releases/download/v$VERSION/tapwing-$VERSION-$HOST_TARGET.tar.gz" \
      "https://github.com/o/r/releases/download/v$VERSION/tapwing-$VERSION-$HOST_TARGET.tar.gz.sha256")
    [[ "$(cat "$WORK/curl.log")" == "$expected_ver_log" ]] || fail "bash -s $VERSION fetched: $(cat "$WORK/curl.log")"
    [[ -x "$PREFIX/versions/$VERSION/bin/tapwing" ]] || fail "bash -s $VERSION did not install $VERSION"
    pass "piped bash < install.sh and bash -s <version> work with env vars"

    # wget fallback when curl is absent (both latest redirect and download).
    WGET_ONLY="$WORK/bin-wgetonly"
    mkdir -p "$WGET_ONLY"
    # Build a PATH without curl by symlinking required POSIX tools (including gzip for GNU tar) + fake wget.
    for tool in sh bash tar gzip gunzip awk sed cut grep cp mv rm mkdir ln chmod dirname basename readlink mktemp uname sysctl tr tail head env cat sha256sum shasum; do
      if tpath=$(command -v "$tool" 2>/dev/null); then
        ln -sf "$tpath" "$WGET_ONLY/$tool"
      fi
    done
    cat >"$WGET_ONLY/wget" <<'WGET'
#!/bin/sh
out="" url="" spider=0
while [ $# -gt 0 ]; do
  case "$1" in
    -O) out=$2; shift 2 ;;
    --spider) spider=1; shift ;;
    -*) shift ;;
    *) url=$1; shift ;;
  esac
done
echo "$url" >>"$TAPWING_TEST_WGET_LOG"
if [ "$spider" = 1 ]; then
  echo "  HTTP/1.1 302 Found" >&2
  echo "  Location: ${url%/latest}/tag/$TAPWING_TEST_LATEST_TAG" >&2
  exit 0
fi
src="$TAPWING_TEST_RELEASE_DIR/$(basename "$url")"
[ -f "$src" ] || exit 8
cp "$src" "$out"
WGET
    chmod +x "$WGET_ONLY/wget"
    : >"$WORK/wget.log"
    rm -rf "$PREFIX/versions/$VERSION"
    if ! wget_out=$(cd "$WORK" && env PATH="$WGET_ONLY" HOME="$TEST_HOME" TAPWING_TEST_WGET_LOG="$WORK/wget.log" \
      TAPWING_TEST_RELEASE_DIR="$WORK/release" TAPWING_TEST_LATEST_TAG="v$VERSION" TAPWING_REPO=o/r \
      TAPWING_NO_MODIFY_PATH=1 \
      "$WGET_ONLY/sh" -s -- --prefix "$PREFIX" --bin-dir "$BINDIR" <"$ROOT/scripts/install.sh" 2>&1); then
      fail "wget fallback install failed: $wget_out"
    fi
    [[ "$(cat "$WORK/wget.log")" == "$expected_log" ]] || fail "wget fallback fetched: $(cat "$WORK/wget.log")"
    [[ -x "$PREFIX/versions/$VERSION/bin/tapwing" ]] || fail "wget fallback did not install $VERSION"
    pass "install.sh falls back to wget when curl is unavailable"

    # Shell rc PATH setup: zsh (idempotent + symlink preservation + clean uninstall), fish, and --no-modify-path.
    RC_HOME="$WORK/rc-home"
    mkdir -p "$RC_HOME/dotfiles" "$RC_HOME/.local/bin"
    printf 'alias ll="ls -la"\n' >"$RC_HOME/dotfiles/zshrc"
    ln -s "$RC_HOME/dotfiles/zshrc" "$RC_HOME/.zshrc"
    RC_PREFIX="$RC_HOME/.tapwing"
    RC_BINDIR="$RC_HOME/.local/bin"

    # --no-modify-path leaves ~/.zshrc untouched.
    HOME="$RC_HOME" SHELL=/bin/zsh "$ROOT/scripts/install.sh" "$HOST_TB" --prefix "$RC_PREFIX" --bin-dir "$RC_BINDIR" --no-modify-path >/dev/null 2>&1
    [[ "$(cat "$RC_HOME/dotfiles/zshrc")" == 'alias ll="ls -la"' ]] || fail "--no-modify-path modified ~/.zshrc"

    # Already on PATH (including via a symlinked directory, e.g. /var -> /private/var on macOS) leaves ~/.zshrc untouched.
    ln -s "$RC_BINDIR" "$RC_HOME/bin-symlink"
    HOME="$RC_HOME" SHELL=/bin/zsh PATH="$RC_HOME/bin-symlink:$PATH" "$ROOT/scripts/install.sh" "$HOST_TB" --prefix "$RC_PREFIX" --bin-dir "$RC_BINDIR" >/dev/null 2>&1
    [[ "$(cat "$RC_HOME/dotfiles/zshrc")" == 'alias ll="ls -la"' ]] || fail "install modified ~/.zshrc even when bin_dir was already on PATH"

    # Not on PATH: updates symlink target in-place, preserving the symlink.
    HOME="$RC_HOME" SHELL=/bin/zsh "$ROOT/scripts/install.sh" "$HOST_TB" --prefix "$RC_PREFIX" --bin-dir "$RC_BINDIR" >/dev/null 2>&1
    [[ -L "$RC_HOME/.zshrc" ]] || fail "install replaced ~/.zshrc symlink with a regular file"
    grep -qx 'export PATH="$HOME/.local/bin:$PATH"' "$RC_HOME/dotfiles/zshrc" || fail "missing PATH export in ~/.zshrc"
    first_rc=$(cat "$RC_HOME/dotfiles/zshrc")

    # Re-running is byte-for-byte idempotent.
    HOME="$RC_HOME" SHELL=/bin/zsh "$ROOT/scripts/install.sh" "$HOST_TB" --prefix "$RC_PREFIX" --bin-dir "$RC_BINDIR" >/dev/null 2>&1
    [[ "$(cat "$RC_HOME/dotfiles/zshrc")" == "$first_rc" ]] || fail "re-running install.sh was not idempotent on ~/.zshrc"

    # --uninstall removes the block and restores the original ~/.zshrc byte-for-byte.
    HOME="$RC_HOME" SHELL=/bin/zsh "$RC_PREFIX/current/install.sh" --uninstall --prefix "$RC_PREFIX" --bin-dir "$RC_BINDIR" >/dev/null 2>&1
    [[ -L "$RC_HOME/.zshrc" ]] || fail "uninstall replaced ~/.zshrc symlink"
    [[ "$(cat "$RC_HOME/dotfiles/zshrc")" == 'alias ll="ls -la"' ]] || fail "uninstall did not restore original ~/.zshrc: $(cat "$RC_HOME/dotfiles/zshrc")"

    # fish shell gets fish_add_path and cleans up on uninstall.
    HOME="$RC_HOME" SHELL=/usr/bin/fish "$ROOT/scripts/install.sh" "$HOST_TB" --prefix "$RC_PREFIX" --bin-dir "$RC_BINDIR" >/dev/null 2>&1
    grep -qx 'fish_add_path "$HOME/.local/bin"' "$RC_HOME/.config/fish/config.fish" || fail "missing fish_add_path in config.fish"
    HOME="$RC_HOME" SHELL=/usr/bin/fish "$RC_PREFIX/current/install.sh" --uninstall --prefix "$RC_PREFIX" --bin-dir "$RC_BINDIR" >/dev/null 2>&1
    if grep -q 'tapwing installer' "$RC_HOME/.config/fish/config.fish"; then
      fail "uninstall left tapwing block in config.fish"
    fi
    pass "install.sh updates shell PATH rc (zsh symlink + idempotency + fish) and removes it on uninstall"

    # From inside an unpacked release with no args: installs that release, no network.
    mkdir -p "$WORK/unpacked-release"
    tar -xzf "$HOST_TB" -C "$WORK/unpacked-release"
    : >"$WORK/curl.log"
    rm -rf "$PREFIX/versions/$VERSION"
    env "${curl_env[@]}" TAPWING_TEST_LATEST_TAG="v9.9.9" \
      "$WORK/unpacked-release/tapwing-$VERSION-$HOST_TARGET/install.sh" --prefix "$PREFIX" --bin-dir "$BINDIR" >/dev/null 2>&1 \
      || fail "install.sh from an unpacked release failed"
    [[ ! -s "$WORK/curl.log" ]] || fail "install.sh from an unpacked release used the network: $(cat "$WORK/curl.log")"
    [[ -x "$PREFIX/versions/$VERSION/bin/tapwing" ]] || fail "install.sh from an unpacked release did not install $VERSION"
    pass "install.sh with no args inside an unpacked release installs it offline"

    # --help works when piped (no script file to read).
    sh -s -- --help <"$ROOT/scripts/install.sh" | grep -q -- "--version VER" || fail "piped install.sh --help printed no usage"
    bash -s -- --help <"$ROOT/scripts/install.sh" | grep -q -- "--no-modify-path" || fail "piped bash install.sh --help missing --no-modify-path"
    pass "install.sh --help works when piped into sh and bash"

    # Uninstall cleans up.
    "$PREFIX/current/install.sh" --uninstall --prefix "$PREFIX" --bin-dir "$BINDIR"
    [[ ! -e "$BINDIR/tapwing" && ! -L "$BINDIR/tapwing" ]] || fail "uninstall left $BINDIR/tapwing"
    [[ ! -e "$PREFIX/current" && ! -e "$PREFIX/versions" ]] || fail "uninstall left files in $PREFIX"
    pass "install.sh --uninstall cleans up cleanly"
    ;;
  *)
    echo "NOTE: skipping host install test for target $HOST_TARGET"
    ;;
esac

# scripts/release.sh: `prepare` puts the version bump on a release branch (main only takes
# pull requests), `tag` tags merged main once CI passed. Runs in a scratch repository with a
# bare origin and stand-ins for cargo and gh, so nothing here touches this checkout or GitHub.
REL="$WORK/release-repo"
REL_BIN="$WORK/release-bin"
mkdir -p "$REL/scripts" "$REL/crates/tapwing" "$REL/bench/parse" "$REL/packages/tapwing_driver" "$REL_BIN"
cp "$ROOT/scripts/release.sh" "$REL/scripts/release.sh"
printf '#!/usr/bin/env bash\necho "stub test-dist.sh"\n' >"$REL/scripts/test-dist.sh"
chmod +x "$REL/scripts/test-dist.sh"
printf '[package]\nname = "tapwing"\nversion = "0.1.0"\n\n[dependencies]\nserde = "1"\n' >"$REL/crates/tapwing/Cargo.toml"
printf '[[package]]\nname = "tapwing"\nversion = "0.1.0"\n\n[[package]]\nname = "serde"\nversion = "1.0.0"\n' >"$REL/crates/tapwing/Cargo.lock"
printf '[package]\nname = "tapwing-parse-bench"\nversion = "0.1.0"\n' >"$REL/bench/parse/Cargo.toml"
printf '[[package]]\nname = "tapwing"\nversion = "0.1.0"\n\n[[package]]\nname = "tapwing-parse-bench"\nversion = "0.1.0"\n' >"$REL/bench/parse/Cargo.lock"
printf 'name: tapwing_driver\nversion: 0.1.0\n' >"$REL/packages/tapwing_driver/pubspec.yaml"
printf '# Driver\n\n```yaml\n    git:\n      path: packages/tapwing_driver\n      ref: v0.1.0\n```\n\nPinned by `ref: v0.1.0` above.\n' >"$REL/packages/tapwing_driver/README.md"
printf '## Unreleased\n\n* A fix.\n\n## 0.1.0\n\n* Initial release.\n' >"$REL/packages/tapwing_driver/CHANGELOG.md"

# cargo: `update --manifest-path <dir>/Cargo.toml` copies the tapwing version into the tapwing
# entry of <dir>/Cargo.lock; `test` exits with $FAKE_CARGO_TEST_RC; `check` succeeds.
cat >"$REL_BIN/cargo" <<'FAKE_CARGO'
#!/usr/bin/env bash
case "${1:-}" in
  update)
    lock="$(dirname "$3")/Cargo.lock"
    v=$(sed -n 's/^version = "\(.*\)"$/\1/p' crates/tapwing/Cargo.toml | head -n1)
    awk -v v="$v" '
      /^name = "tapwing"$/ { pkg = 1; print; next }
      pkg && /^version = "/ { print "version = \"" v "\""; pkg = 0; next }
      { print }
    ' "$lock" >"$lock.new"
    mv "$lock.new" "$lock"
    ;;
  test) exit "${FAKE_CARGO_TEST_RC:-0}" ;;
esac
FAKE_CARGO
# gh: logs its arguments; `api` answers the CI check state from $FAKE_CI_STATE.
cat >"$REL_BIN/gh" <<'FAKE_GH'
#!/usr/bin/env bash
echo "$*" >>"$TAPWING_TEST_GH_LOG"
case "${1:-}" in
  api) echo "${FAKE_CI_STATE:-success}" ;;
  pr) echo "https://github.com/o/r/pull/1" ;;
esac
FAKE_GH
chmod +x "$REL_BIN/cargo" "$REL_BIN/gh"

git init --quiet --bare "$WORK/release-origin.git"
git -C "$REL" init --quiet
git -C "$REL" symbolic-ref HEAD refs/heads/main
git -C "$REL" config user.name "Release Test"
git -C "$REL" config user.email "release-test@example.invalid"
git -C "$REL" config commit.gpgsign false
git -C "$REL" config tag.gpgsign false
git -C "$REL" add -A
git -C "$REL" commit --quiet -m "initial"
git -C "$REL" remote add origin "$WORK/release-origin.git"
git -C "$REL" push --quiet -u origin main

release() {
  (cd "$REL" && PATH="$REL_BIN:$PATH" TAPWING_TEST_GH_LOG="$WORK/gh.log" scripts/release.sh "$@")
}
rel_clean_on_main() {
  [[ "$(git -C "$REL" symbolic-ref --short HEAD)" == main ]] || fail "release.sh $1: not back on main"
  [[ -z "$(git -C "$REL" status --porcelain --untracked-files=all)" ]] || fail "release.sh $1: left changes behind"
}
rel_has_ref() { git -C "$REL" rev-parse --quiet --verify "$1" >/dev/null; }
: >"$WORK/gh.log"

release prepare 0.1.1 --dry-run >"$WORK/release.out" 2>&1 || fail "release.sh prepare --dry-run failed: $(cat "$WORK/release.out")"
rel_clean_on_main "prepare --dry-run"
if rel_has_ref refs/heads/release/v0.1.1; then fail "release.sh prepare --dry-run created a branch"; fi
grep -q "git switch -c release/v0.1.1" "$WORK/release.out" || fail "release.sh prepare --dry-run did not print its plan"
pass "release.sh prepare --dry-run changes nothing"

if release tag 0.1.1 >"$WORK/release.out" 2>&1; then fail "release.sh tag tagged a main that was not prepared"; fi
grep -q "not prepared for v0.1.1" "$WORK/release.out" || fail "release.sh tag: unclear error on an unprepared main: $(cat "$WORK/release.out")"
pass "release.sh tag refuses a main that does not carry the version"

if FAKE_CARGO_TEST_RC=1 release prepare 0.1.1 >"$WORK/release.out" 2>&1; then fail "release.sh prepare ignored a failing verification"; fi
rel_clean_on_main "prepare with a failing check"
if rel_has_ref refs/heads/release/v0.1.1; then fail "release.sh prepare kept its branch after a failed check"; fi
grep -qx 'version = "0.1.0"' "$REL/crates/tapwing/Cargo.toml" || fail "release.sh prepare did not restore Cargo.toml"
pass "release.sh prepare undoes its changes when verification fails"

release prepare 0.1.1 >"$WORK/release.out" 2>&1 || fail "release.sh prepare failed: $(cat "$WORK/release.out")"
rel_clean_on_main "prepare"
grep -qx 'version = "0.1.0"' "$REL/crates/tapwing/Cargo.toml" || fail "release.sh prepare changed main"
rb=origin/release/v0.1.1
git -C "$REL" fetch --quiet origin
[[ "$(git -C "$REL" log -1 --format=%s "$rb")" == "release: v0.1.1" ]] || fail "release.sh prepare did not push its commit"
[[ "$(git -C "$REL" rev-list --count "main..$rb")" == 1 ]] || fail "release.sh prepare pushed more than the release commit"
git -C "$REL" show "$rb:crates/tapwing/Cargo.toml" | grep -qx 'version = "0.1.1"' || fail "release branch: Cargo.toml not bumped"
git -C "$REL" show "$rb:crates/tapwing/Cargo.lock" | grep -A1 '^name = "tapwing"$' | grep -qx 'version = "0.1.1"' || fail "release branch: Cargo.lock not bumped"
git -C "$REL" show "$rb:crates/tapwing/Cargo.lock" | grep -A1 '^name = "serde"$' | grep -qx 'version = "1.0.0"' || fail "release branch: Cargo.lock bumped another crate"
git -C "$REL" show "$rb:bench/parse/Cargo.lock" | grep -A1 '^name = "tapwing"$' | grep -qx 'version = "0.1.1"' || fail "release branch: bench/parse/Cargo.lock not bumped"
git -C "$REL" show "$rb:bench/parse/Cargo.lock" | grep -A1 '^name = "tapwing-parse-bench"$' | grep -qx 'version = "0.1.0"' || fail "release branch: bench/parse/Cargo.lock bumped the bench crate"
git -C "$REL" show "$rb:packages/tapwing_driver/pubspec.yaml" | grep -qx 'version: 0.1.1' || fail "release branch: pubspec.yaml not bumped"
git -C "$REL" show "$rb:packages/tapwing_driver/README.md" | grep -qx '      ref: v0.1.1' || fail "release branch: README.md ref not bumped"
git -C "$REL" show "$rb:packages/tapwing_driver/README.md" | grep -qxF 'Pinned by `ref: v0.1.0` above.' || fail "release branch: README.md changed outside the install snippet"
git -C "$REL" show "$rb:packages/tapwing_driver/CHANGELOG.md" | grep -qx '## 0.1.1' || fail "release branch: CHANGELOG.md has no 0.1.1 entry"
if git -C "$REL" show "$rb:packages/tapwing_driver/CHANGELOG.md" | grep -q 'Unreleased'; then fail "release branch: CHANGELOG.md still says Unreleased"; fi
grep -q "^pr create --base main --head release/v0.1.1 --title release: v0.1.1" "$WORK/gh.log" || fail "release.sh prepare did not open a pull request: $(cat "$WORK/gh.log")"
pass "release.sh prepare pushes one release commit on release/v0.1.1 and opens its pull request"

if release prepare 0.1.1 >"$WORK/release.out" 2>&1; then fail "release.sh prepare ran twice for one version"; fi
grep -q "already exists" "$WORK/release.out" || fail "release.sh prepare: unclear error for an existing branch"
pass "release.sh prepare refuses an existing release branch"

# The pull request is merged: main moves to the release commit on origin, then locally.
git -C "$REL" push --quiet origin "$rb:refs/heads/main"
git -C "$REL" pull --quiet --ff-only origin main
git -C "$REL" branch --quiet -D release/v0.1.1

if FAKE_CI_STATE=failure release tag 0.1.1 >"$WORK/release.out" 2>&1; then fail "release.sh tag ignored a failed CI Gate"; fi
grep -q '"CI Gate" check' "$WORK/release.out" || fail "release.sh tag: unclear error for a failed CI Gate: $(cat "$WORK/release.out")"
if rel_has_ref refs/tags/v0.1.1; then fail "release.sh tag tagged despite a failed CI Gate"; fi
pass "release.sh tag refuses a commit whose CI Gate did not pass"

release tag 0.1.1 --dry-run >"$WORK/release.out" 2>&1 || fail "release.sh tag --dry-run failed: $(cat "$WORK/release.out")"
if rel_has_ref refs/tags/v0.1.1; then fail "release.sh tag --dry-run created the tag"; fi
release tag 0.1.1 >"$WORK/release.out" 2>&1 || fail "release.sh tag failed: $(cat "$WORK/release.out")"
[[ "$(git -C "$REL" rev-parse 'v0.1.1^{commit}')" == "$(git -C "$REL" rev-parse main)" ]] || fail "release.sh tag did not tag main"
git -C "$REL" ls-remote --exit-code --tags origin refs/tags/v0.1.1 >/dev/null || fail "release.sh tag did not push the tag"
rel_clean_on_main "tag"
pass "release.sh tag tags merged main and pushes the tag"

if release tag 0.1.1 >"$WORK/release.out" 2>&1; then fail "release.sh tag tagged the same version twice"; fi
grep -q "already exists" "$WORK/release.out" || fail "release.sh tag: unclear error for an existing tag"
git -C "$REL" commit --quiet --allow-empty -m "local only"
if release prepare 0.1.2 >"$WORK/release.out" 2>&1; then fail "release.sh released commits that are not on origin/main"; fi
grep -q "ahead of origin/main" "$WORK/release.out" || fail "release.sh: unclear error when main is ahead: $(cat "$WORK/release.out")"
pass "release.sh refuses an existing tag and a main that is ahead of origin"

echo "ALL DIST/INSTALL TESTS PASSED"

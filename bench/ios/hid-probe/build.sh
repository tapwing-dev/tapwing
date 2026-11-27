#!/usr/bin/env bash
# Builds hid-probe into bench/ios/hid-probe/build/ (gitignored). No private framework is linked:
# shim.m loads them with dlopen at run time.
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
shim=$here/../../../crates/tapwing/native/ios_hid
mkdir -p "$here/build"
clang -fobjc-arc -Wall -Wno-unused-function -O1 -I"$shim" \
  "$here/main.m" "$shim/shim.m" \
  -framework Foundation -framework CoreGraphics -framework IOSurface -framework ImageIO \
  -o "$here/build/hid-probe"
echo "built $here/build/hid-probe"

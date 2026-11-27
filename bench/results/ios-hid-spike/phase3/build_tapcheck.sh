#!/usr/bin/env bash
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
shim=$here/../../../../crates/tapwing/native/ios_hid
mkdir -p "$here/build"
clang -fobjc-arc -Wall -Wno-unused-function -O1 -I"$shim" "$here/tapcheck.m" "$shim/shim.m" \
  -framework Foundation -framework CoreGraphics -framework IOSurface -framework ImageIO -o "$here/build/tapcheck"

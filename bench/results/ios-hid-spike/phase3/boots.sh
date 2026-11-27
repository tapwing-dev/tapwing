#!/usr/bin/env bash
# Task 4b: delivery per boot on a temporary simulator. Usage: boots.sh <boots> <taps>
set -u
here=$(cd "$(dirname "$0")" && pwd)
repo=$(cd "$here/../../../.." && pwd)
out=$here/reliability
app=$repo/examples/sample_app/build/ios/iphonesimulator/Runner.app
dev=$(xcrun simctl create Tapwing-HID-Spike com.apple.CoreSimulator.SimDeviceType.iPhone-18-Pro-Max com.apple.CoreSimulator.SimRuntime.iOS-27-2)
echo "$dev" > "$out/temp_udid.txt"
echo "created $dev"
for b in $(seq 1 "$1"); do
  t0=$(date +%s)
  xcrun simctl boot "$dev" && xcrun simctl bootstatus "$dev" -b >/dev/null 2>&1
  echo "boot $b ready after $(( $(date +%s) - t0 )) s"
  xcrun simctl install "$dev" "$app"
  (cd "$repo" && DEV=$dev ./crates/tapwing/target/release/tapwing test bench/results/ios-hid-spike/phase3/flows/nav_detail.yaml --device "$dev" > "$out/boot$b.nav.log" 2>&1) || echo "nav flow failed on boot $b"
  sleep 3
  "$here/build/tapcheck" "$dev" "$2" 10 369 197 1070 592 > "$out/boot$b.csv" 2> "$out/boot$b.err"
  awk -F, '{c[$5]++} END{for(k in c) printf "%s=%d ", k, c[k]; print ""}' "$out/boot$b.csv"
  xcrun simctl shutdown "$dev"
done
xcrun simctl delete "$dev"
echo deleted

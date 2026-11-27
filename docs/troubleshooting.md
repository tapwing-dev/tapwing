# Troubleshooting

Common setup, device, and runtime issues when running Tapwing flows.

## Android `adb` Connection Drops

### Flows fail with `app closed the connection` or `app did not connect` (adb 37.0.1)

Android platform-tools 37.0.1 has a bug where mDNS auto-discovery crashes the `adb` server (`SIGABRT` in `adbmdns_bridge::zero_config`) when emulators advertise themselves, which drops every `adb reverse` tunnel.

Disable mDNS discovery and restart `adb`:

```sh
export ADB_MDNS=0
adb kill-server
```

Add `export ADB_MDNS=0` to your shell profile (and run `launchctl setenv ADB_MDNS 0` on macOS for GUI apps). USB devices and manual `adb connect` keep working normally.

## App Fails to Connect to `tapwing_driver`

If `launchApp` runs for 15 seconds with no driver connection, Tapwing switches that `appId` to black-box native mode (or fails immediately if `--input flutter` was passed). Check:

1. **Right entrypoint built:** Verify the APK or `.app` was built with `-t lib/main_tapwing.dart` (not production `lib/main.dart`).
2. **Release build guard:** In release builds `tapwing_driver` stays off by default. Use `--profile` (Android) / `--debug` (iOS Simulator), or pass `--dart-define=TAPWING_ALLOW_RELEASE=true` when testing a release build intentionally ([setup.md](setup.md)).
3. **Another simulator/device responded first:** Tapwing rejects handshakes from a different platform, and you can inspect port/driver selection with `TAPWING_DEBUG=1`.
4. **Screen is PIN-locked:** Tapwing wakes Android screens automatically, but a PIN-locked device fails immediately.

## `setupWarning`: Flutter `"Zone mismatch."`

If your app calls `runZonedGuarded`, `runZoned`, or `SentryFlutter.init(..., appRunner: ...)` inside `main()`, initializing `TapwingBinding.ensureInitialized()` before `app.main()` creates a root-zone binding while `runApp()` runs in a child zone.

- This is surfaced as a non-blocking `setupWarning` and will **not** fail `--app-errors fail`.
- Move binding initialization inside a shared `bootstrap()` helper if you want both zones unified or if `BindingBase.debugZoneErrorsAreFatal = true`. See [setup.md](setup.md#zones-and-error-wrappers).

## Settling Timeouts on Background Requests or Polling

Tapwing waits for in-flight `dart:io` HTTP requests and image loads before considering a screen settled, capped by default at 5 seconds per request/image.

- Increase or disable the cap with `--settle-load-timeout-ms <ms>` (`0` = unbounded) or `tapwing.settleLoadTimeoutMs` in `config.yaml`.
- Exclude noisy analytics, telemetry, or long-polling URLs with repeatable `--settle-http-ignore <regex>` or `tapwing.settleHttpIgnore` in `config.yaml`.

## Native Lanes (Android & iOS Simulator)

- **Inspect which driver binary Tapwing chose:** Run with `TAPWING_DEBUG=1 tapwing test ...` (or `tapwing --version`) to verify whether bundled drivers or `TAPWING_DRIVER_APK` / `TAPWING_IOS_DRIVER_DIR` overrides are active ([install.md](install.md#how-drivers-are-found)).
- **Xcode version mismatch warning on iOS Simulator:** When `tapwing-ios-runner.build-info.json` reports a different Xcode major version from local `xcodebuild -version`, Tapwing prints a warning and tries running anyway. If XCUITest fails to start, match `xcode-select` or rebuild locally via `scripts/build-ios-driver.sh` and set `TAPWING_IOS_DRIVER_DIR=drivers/ios/build`.
- **iOS `hideKeyboard` with focused Flutter field:** Flutter on iOS keeps field focus when tapping outside by default; Tapwing dismisses the keyboard with a warning unless `--maestro-compat strict` is set ([commands.md](commands.md#text-clipboard-and-keys)).
- **Wedged iOS runner or leftover processes:** Each XCUITest request carries a deadline; after two consecutive timeouts Tapwing restarts the runner (up to 3 restarts per run). Leftover `/sdcard/tapwing_rec_<pid>_*.mp4` recordings or `xcodebuild` scratch runners from hard-killed (`SIGKILL`) runs are cleaned automatically on the next session ([devices.md](devices.md), [native-lanes.md](native-lanes.md)).

## Missing Element or Unexpected Screen State

Run with `--trace` (and `--trace-screenshots steps` for per-step screenshots) or `--app-errors-screenshot` to generate an offline HTML timeline showing every command lane, redacted app HTTP requests, app errors, UI tree dumps, and "Did you mean" selector hints. See [failure-diagnostics.md](failure-diagnostics.md).

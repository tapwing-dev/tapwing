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

### A tap that opens a system alert or system sheet on iOS takes 3 s

A tap whose handler calls a plugin and waits for the answer, such as a permission request, leaves a platform channel call pending until the user answers the alert. Tapwing counts a pending platform channel call as busy, so the settle after the tap waits for the reply. On Android the system dialog pauses the app and the settle ends at once. On the iOS Simulator (seen on iOS 27.2) the app stays active under a SpringBoard alert, Flutter reports no lifecycle change, and the settle runs for its whole timeout, 3 s by default.

The step passes, and the console names what the settle waited for:

```text
  ✓  3017.0 ms  tapOn "Request Location Permission"
      ⚠ settle timed out, still waiting for platform channel dev.tapwing.sample_app/permissions (pending 3003 ms)
```

The JSON report (`--report`) and `trace.json` carry it in the step's `driver` object as `settleBusy`, next to `settle` and `t.settleUs`. The trace viewer shows the console line in the step.

```json
"driver": {
  "settle": "timeout",
  "t": { "findUs": 1244, "actUs": 13686, "settleUs": 3003736 },
  "settleBusy": [
    { "source": "channel", "channel": "dev.tapwing.sample_app/permissions", "pendingMs": 3003 }
  ]
}
```

`settleBusy` is present only when a settle timed out. Each entry names one thing that was still busy at that moment.

| `source` | Meaning |
|---|---|
| `channel` | A platform channel call awaits its reply. `channel` is its name, `pendingMs` is how long the oldest call has waited, and `calls` is their number when above 1. Tapwing knows the channel name only, never the method or its arguments. |
| `http` | `count` HTTP requests in flight (see the section above). |
| `images` | `count` images loading. |
| `keyboard` | The soft keyboard is still showing or hiding. |
| `animation` | A large animation is running. The entry has its `widget`, the `fraction` of the screen it covers and its `status`. |
| `frames` | The UI keeps changing from frame to frame. |

A `channel` entry on the tap that opens the alert or sheet identifies this case. The field needs a `tapwing_driver` that sends it. With an older driver the step looks as before, without the line and without the field.

To stop waiting, give that one step a shorter settle timeout:

```yaml
- tapOn:
    text: "Request Location Permission"
    waitToSettleTimeoutMs: 300
```

The setting applies to that step only. The next step, an `assertVisible` on the alert, waits for the alert by itself.

Measured on the sample app (`examples/sample_app/.maestro/ios/f5_permission_dialog.yaml`, iPhone 18 Pro Max simulator, iOS 27.2) over 5 runs each, the step takes 318 to 326 ms with the setting and 3,013 to 3,023 ms without it.

The default is unchanged. The settle timeout stays 3 s, Tapwing does not end a settle early because a channel call is pending, and no channel gets special treatment, so without the per-step setting the step takes 3 s as before. Ending the wait when the alert appears needs a signal from the host and is tracked in [#20](https://github.com/tapwing-dev/tapwing/issues/20).

With `retryTapIfNoChange: true` the tap is still delivered once. A tap that started a platform channel call which is still pending is not repeated, because the second tap would reach the widget under the alert.

## Native Lanes (Android & iOS Simulator)

- **Inspect which driver binary Tapwing chose:** Run with `TAPWING_DEBUG=1 tapwing test ...` (or `tapwing --version`) to verify whether bundled drivers or `TAPWING_DRIVER_APK` / `TAPWING_IOS_DRIVER_DIR` overrides are active ([install.md](install.md#how-drivers-are-found)).
- **Xcode version mismatch warning on iOS Simulator:** When `tapwing-ios-runner.build-info.json` reports a different Xcode major version from local `xcodebuild -version`, Tapwing prints a warning and tries running anyway. If XCUITest fails to start, match `xcode-select` or rebuild locally via `scripts/build-ios-driver.sh` and set `TAPWING_IOS_DRIVER_DIR=drivers/ios/build`.
- **iOS `hideKeyboard` with focused Flutter field:** Flutter on iOS keeps field focus when tapping outside by default; Tapwing dismisses the keyboard with a warning unless `--maestro-compat strict` is set ([commands.md](commands.md#text-clipboard-and-keys)).
- **Wedged iOS runner or leftover processes:** Each XCUITest request carries a deadline; after two consecutive timeouts Tapwing restarts the runner (up to 3 restarts per run). Leftover `/sdcard/tapwing_rec_<pid>_*.mp4` recordings or `xcodebuild` scratch runners from hard-killed (`SIGKILL`) runs are cleaned automatically on the next session ([devices.md](devices.md), [native-lanes.md](native-lanes.md)).

## Missing Element or Unexpected Screen State

Run with `--trace` (and `--trace-screenshots steps` for per-step screenshots) or `--app-errors-screenshot` to generate an offline HTML timeline showing every command lane, redacted app HTTP requests, app errors, UI tree dumps, and "Did you mean" selector hints. See [failure-diagnostics.md](failure-diagnostics.md).

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

What happens when `launchApp` gets no driver connection depends on what Tapwing could read in the installed build ([native-lanes.md](native-lanes.md#skipping-the-handshake-wait-for-a-build-without-the-driver)):

| The installed build | In `--input auto` |
|---|---|
| has no driver | No wait. The app is driven black-box through the native lane, with a warning. |
| could not be read, or links the driver without its dial path | After 15 seconds the flow drives the app black-box. Two misses in a row make that last for the rest of the run. |
| contains a driver that dials the host | Never black-box. While the app process is running the wait goes on until 60 seconds after the launch, then `launchApp` fails with `the installed build contains tapwing_driver (...) but the driver never connected`. A process that died fails the step at once as a crash. |

`--input flutter` waits 20 seconds and fails in every case. `--input native` never waits for the driver.

### `the installed build contains tapwing_driver (...) but the driver never connected`

The build is right, so look at the connection and at the start of the app:

1. **The `adb reverse` tunnel is gone (Android):** An `adb` server restart drops every tunnel, see [Android `adb` Connection Drops](#android-adb-connection-drops). `adb -s <serial> reverse --list` shows what is left. Tapwing sets the tunnel up once when a run starts, so start the run again.
2. **The app starts too slowly:** On a busy host or a software-rendered emulator a cold start can take longer than 60 seconds. Lower the load, use fewer shards, or give the emulator more cores. The error says whether the app process was still running. Android also ends a process that takes too long: on an Android 15 emulator it killed one that had not completed its startup after about 25 seconds (`failed to complete startup` in `adb logcat`), which Tapwing reports as an app crash at `launchApp`.
3. **The app hangs before `main()` finishes:** `TapwingBinding.ensureInitialized()` in `lib/main_tapwing.dart` starts the driver. Code that blocks before it delays the handshake. `adb logcat` (Android) or the simulator log shows where the app stops.
4. **Another simulator/device responded first:** Tapwing rejects handshakes from a different platform, and you can inspect port/driver selection with `TAPWING_DEBUG=1`.
5. **Screen is PIN-locked:** Tapwing wakes Android screens automatically, but a PIN-locked device fails immediately.

A later error that says `was not repeated` means an earlier launch of the same app already waited the 60 seconds in this run. Tapwing then waits 15 seconds per launch until a handshake arrives, so a broken setup does not cost a minute per flow. To drive the app black-box on purpose, pass `--input native`.

### The app went black-box and you expected the driver

1. **Right entrypoint built:** Verify the APK or `.app` was built with `-t lib/main_tapwing.dart` (not production `lib/main.dart`).
2. **Release build guard:** In release builds `tapwing_driver` stays off by default. Use `--profile` (Android) / `--debug` (iOS Simulator), or pass `--dart-define=TAPWING_ALLOW_RELEASE=true` when testing a release build intentionally ([setup.md](setup.md)).
3. **The build could not be read:** `TAPWING_DEBUG=1` prints the probe's verdict and what it rests on. With an unknown verdict a slow start still ends in black-box mode after 15 seconds.

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

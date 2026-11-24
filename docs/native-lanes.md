# Native lanes

Flutter's semantics tree only sees widgets inside your Flutter app. System permission dialogs, WebViews, notifications, other apps, and iOS system actions (`Home`, `Lock`, rotation) live outside it.

Tapwing handles those with its own bundled native drivers: a UiAutomator driver on Android and an XCUITest runner on the iOS Simulator (both ported from Maestro, Apache-2.0). No Maestro installation is needed.

## Choosing a mode (`--input`)

```sh
tapwing test .maestro/permissions.yaml                      # auto (default)
tapwing test .maestro/login.yaml --input flutter            # in-app Flutter driver only
tapwing test .maestro/other_app.yaml --input native         # native driver only (any app)
```

| Mode | Behavior |
|---|---|
| `auto` (default) | Uses the fast in-app Flutter driver and falls back to the native driver when needed |
| `flutter` | Uses only the in-app Flutter driver; never starts the native lane |
| `native` | Drives every step through the native driver; works on apps without `tapwing_driver` |

## How `auto` routing works

On each step, Tapwing tries Flutter first so normal app UI stays fast. If an element isn't found, it checks which window currently has focus. When a system dialog, WebView, or another app is on top, it routes the step to the native driver without losing the remaining timeout. Once focus returns to your app, Tapwing switches back to Flutter automatically.

Commands that can be affected by overlays upfront (`assertNotVisible`, `back`, `scrollUntilVisible`) check window focus right away.

**Apps without `tapwing_driver`.** If `launchApp` receives no driver handshake within 15 seconds (or a flow attaches to an uninstrumented app), Tapwing automatically marks that `appId` as black-box and drives it through the native lane for the rest of the run.

## Android and iOS Simulator drivers

- **Android (UiAutomator).** Tapwing ships a single helper APK (`dev.tapwing.driver`) that is installed on the device only when missing or updated. It warms up in the background so pure-Flutter flows never wait for it, and supports Unicode `inputText` without changing your device's keyboard IME. Internals: [drivers/android/README.md](../drivers/android/README.md).
- **iOS Simulator (XCUITest).** Tapwing ships a small universal XCUITest runner (`dev.tapwing.ios-runner`). Pure-Flutter flows never launch `xcodebuild`, so they pay zero startup cost. Flows that use `setOrientation`, `pressKey: Home|Lock`, or `--input native` warm it up in the background; unexpected SpringBoard permission alerts start it on demand. Internals: [drivers/ios/README.md](../drivers/ios/README.md).

## Reconnects and slow cold starts

**Mid-run disconnects.** If the connection to a native driver breaks (an `adb` server restart, a USB hiccup, or a killed runner process), Tapwing tears down the dead session, starts a fresh one (up to 3 restarts per run, with a warning), and retries the command only when it is safe to do so:

- Reads are always retried.
- Taps, text input, and key presses are retried only if the connection was refused before the request could be sent. Otherwise the step fails cleanly instead of risking a duplicate tap, and the lane is ready again for the next step.

Mock-location providers created by `setLocation` or `travel` on Android are cleaned up when the session ends.

**Slow cold starts.** On a busy machine, a freshly cleared app can take a while to come up. If the first handshake after `launchApp` gets no answer for 10 seconds, Tapwing reconnects and gives the same process one more chance (up to 20 seconds more). On Android in native or black-box mode, if the screen is still empty after launch, the first required lookup gets an extra 10-second window (disabled under `--maestro-compat strict`).

## Environment variables and reports

| Variable | Default | Purpose |
|---|---|---|
| `TAPWING_DRIVER_APK` | bundled | Override the Android native driver APK path |
| `TAPWING_IOS_DRIVER_DIR` | bundled | Override the unpacked iOS native driver directory |
| `TAPWING_NATIVE_STARTUP_TIMEOUT_MS` | `20000` | Android native driver startup timeout (ms) |
| `TAPWING_IOS_NATIVE_STARTUP_TIMEOUT_MS` | `60000` | iOS XCUITest runner startup timeout (ms) |
| `TAPWING_NATIVE_RESTART_TIMEOUT_MS` | `30000` | How long Android waits for a device to return after a disconnect (ms) |

In JSON reports (`--report`), each flow records its `lane` (`flutter`, `native`, or `hybrid`), per-lane command counts (`laneStats`), `blackbox: true` when no in-app driver connected, and `coldStartRetries` if any occurred.

*Note:* `--native-driver` and `tapwing.nativeDriver` in `config.yaml` are deprecated no-ops. Tapwing always uses its own native drivers.

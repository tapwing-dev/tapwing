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

**Apps without `tapwing_driver`.** If `launchApp` receives no driver handshake within 15 seconds (or a flow attaches to an uninstrumented app), Tapwing marks that `appId` as black-box for the flow and drives it through the native lane. Two misses in a row in one run make that last for the rest of the run. Most of the time Tapwing knows earlier than that, and does not wait at all (next section).

## Skipping the handshake wait for a build without the driver

In `--input auto`, `launchApp` looks inside the installed app while the app starts, on its own thread and with its own 5-second deadline, so an instrumented app is not slowed down. When the app provably has no driver, the 15-second wait ends at once, the flow drives the app through the native lane, and the iOS XCUITest runner starts immediately instead of after the wait. Tapwing says so on stderr:

```text
warning: launchApp com.example.app: the installed build contains no tapwing_driver (read base.apk: lib/arm64-v8a/libapp.so (7.3 MB) completely: no tapwing_driver marker); not waiting 15 s for a handshake that cannot come, driving it black-box via the native lane (pass --input flutter to wait for the driver anyway)
```

The same reason is in the reports: `blackbox: true` and `blackboxReason` on the flow in run.json and the trace, a `driverProbe` object (`verdict`, `source`, `evidence`) on the `launchApp` step, the reason next to `blackbox` in the HTML report and under `Black-box:` in the JUnit `<system-err>`.

**What counts as proof.** The probe looks for the literals of the driver (`tapwing_driver_hello_v1` and the older `tapwing_port`, `TAPWING_PORT`, `TAPWING_ALLOW_RELEASE` and `package:tapwing_driver/`) in the Dart payload of the build. Dart never obfuscates string literals and compiles in only what the entrypoint reaches, so a build made from the plain `lib/main.dart` has none of them, while `lib/main_tapwing.dart` has them in debug, profile and release with `--dart-define=TAPWING_ALLOW_RELEASE=true`, with or without `--obfuscate`.

| Where the payload is read | What is read |
|---|---|
| Android profile and release | `lib/<abi>/libapp.so`, stored uncompressed in the APK, read on the device with `unzip -p … \| grep` through adb |
| Android debug | `assets/flutter_assets/kernel_blob.bin`, deflated in the APK, inflated on the device (about a second) |
| Android app bundle splits | every APK `pm path` lists, so a payload in `split_config.<abi>.apk` is found |
| Android, libraries extracted next to the APK | `<codePath>/lib/<abi>/libapp.so` in place |
| iOS Simulator | `Frameworks/App.framework/flutter_assets/kernel_blob.bin` and `Frameworks/App.framework/App` of the installed `.app`, read by Tapwing itself |
| Any app without the Flutter engine | none: an app with no `libflutter.so` (Android) or no `Flutter.framework` (iOS) is not a Flutter app |

The verdict is **absent** only when the payload was found, read completely (the byte count must match the file) and holds none of those literals. A marker anywhere is **present**. Everything else is **unknown** and behaves exactly as before, with the 15-second wait: a payload that could not be found or read in full, a split or listing the device would not give, `unzip` or `grep` missing on the device, a deferred Dart component, more than 128 MB of payload, a deadline, or any error. A wrong "present" only costs the old wait. A release build of `main_tapwing.dart` without `TAPWING_ALLOW_RELEASE` still links the driver package without its dial path, so it is judged present and waits as before (the driver itself prints that it is disabled).

On `emulator-5554` (Android 15, profile APK) a plain build's `launchApp` went from 15.4 s to 1.8 s (median of 5, alternating runs); on an iPhone 18 Pro simulator (debug build) from 20.2 s to 3.9 s. A build with the driver is not slowed: the same flows show no measurable change.

`--input flutter` never probes and never skips: it is how you insist on the driver. `--input native` never reaches the driver at all. Set `TAPWING_DEBUG=1` to print each verdict with the time it took.

**What runs remember.** Verdicts and handshake misses are kept per device, app id and install in `~/.tapwing/driver-probe.json` (written atomically, at most 256 entries, entries older than 90 days dropped; a missing or corrupt file is ignored and rewritten, so deleting it is always safe). The install is identified on Android by the path, size and modification time of every APK (the path changes with every `adb install`), on iOS by the code-signature resource lists of the bundle, which depend on its content only and so survive `clearState`. A rebuilt app is a new install and is read again. When a build cannot be read, Tapwing also counts the launches that waited the full 15 seconds with the app still running (a crashing or never-started app is not counted) and got no handshake. After two in a row it stops waiting for that install and says so, until a handshake clears the count or a week has passed. The count is never acted on for a build the probe could show to dial in.

## Android and iOS Simulator drivers

- **Android (UiAutomator).** Tapwing ships a single helper APK (`dev.tapwing.driver`) that is installed on the device only when missing or updated. It warms up in the background so pure-Flutter flows never wait for it, and supports Unicode `inputText` without changing your device's keyboard IME. Internals: [drivers/android/README.md](../drivers/android/README.md).
- **iOS Simulator (XCUITest).** Tapwing ships a small universal XCUITest runner (`dev.tapwing.ios-runner`). Pure-Flutter flows never launch `xcodebuild`, so they pay zero startup cost. Flows that use `setOrientation`, `pressKey: Home|Lock`, or `--input native` warm it up in the background; unexpected SpringBoard permission alerts start it on demand. Internals: [drivers/ios/README.md](../drivers/ios/README.md).

## Reconnects and slow cold starts

**Mid-run disconnects.** If the connection to a native driver breaks (an `adb` server restart, a USB hiccup, or a killed runner process), Tapwing tears down the dead session, starts a fresh one (up to 3 restarts per run, with a warning), and retries the command only when it is safe to do so:

- Reads are always retried.
- Taps, text input, and key presses are retried only if the request provably never left your machine: the connection was refused, or (on Android, where a dead driver behind `adb forward` still accepts connections) it was closed before the driver sent anything and before Tapwing wrote a byte. Otherwise the step fails cleanly instead of risking a duplicate tap, and the lane is ready again for the next step.

On Android a restart is bounded in time as well as in number: each one may take up to 30 seconds (it waits for a device that is briefly `offline` to come back), and all restarts of a run share 60 seconds. The error names the budget when it runs out. A start that cannot succeed (no driver APK, `adb install` refused it, the instrumentation crashed) is not retried and fails at once with that cause.

**A runner that stops answering (iOS Simulator).** XCUITest work cannot be cancelled, so a call that never returns would leave the runner alive but stuck. Every request therefore carries a deadline a little below Tapwing's 30-second request timeout: the runner answers with a timeout error instead of going silent, refuses further requests while the stuck call is still running, and reports the state in `GET /status`. One timed-out call only fails its step. After two in a row Tapwing restarts the runner (within the same budget of 3 restarts per run, with a warning) and repeats the command under the same rule as above: reads always, input only when the runner reports that it never started it.

**A runner built with another Xcode (iOS Simulator).** The prebuilt runner records the Xcode and iOS Simulator SDK it was built with (`tapwing-ios-runner.build-info.json` next to it). When the native lane starts, Tapwing compares that with the local `xcodebuild -version` and prints one `warning:` line if the major versions differ. It is advisory only: the run continues, and nothing is printed for a runner without that file. If the lane then fails to start, select a matching Xcode with `xcode-select`, or build the runner yourself with `scripts/build-ios-driver.sh` and point `TAPWING_IOS_DRIVER_DIR` at `drivers/ios/build`.

**A leftover runner (iOS Simulator).** If an earlier run was killed hard, its runner can still be listening, which matters when `TAPWING_IOS_NATIVE_PORT` pins the port. Tapwing starts every runner with a fresh session id and waits until `/status` reports that id, so it never drives a leftover runner (possibly on another simulator). If the leftover one keeps the port, startup fails with a message naming it instead of silently using it. A runner built before session ids existed still works, with one warning.

Typing is the one request that may legitimately take longer: the deadline of `inputText` and `eraseText` on the iOS native lane grows with the text (30 seconds plus one second per 15 characters). The runner types long text in chunks and stops between them once the deadline has passed or Tapwing has disconnected, so it never keeps typing into the app after the step has failed.

Mock-location providers created by `setLocation` or `travel` on Android are cleaned up when the session ends.

**Slow cold starts.** On a busy machine, a freshly cleared app can take a while to come up. If the first handshake after `launchApp` gets no answer for 10 seconds, Tapwing reconnects and gives the same process one more chance (up to 20 seconds more). On Android in native or black-box mode, if the screen is still empty after launch, the first required lookup gets an extra 10-second window (disabled under `--maestro-compat strict`).

## Environment variables and reports

| Variable | Default | Purpose |
|---|---|---|
| `TAPWING_DRIVER_APK` | bundled | Override the Android native driver APK path |
| `TAPWING_IOS_DRIVER_DIR` | bundled | Override the unpacked iOS native driver directory |
| `TAPWING_NATIVE_STARTUP_TIMEOUT_MS` | `20000` | Android native driver startup timeout (ms) |
| `TAPWING_IOS_NATIVE_STARTUP_TIMEOUT_MS` | `60000` | iOS XCUITest runner startup timeout (ms) |
| `TAPWING_NATIVE_RESTART_TIMEOUT_MS` | `30000` | How long one Android driver restart may take, including the wait for a device to return after a disconnect (ms) |
| `TAPWING_DEBUG` | unset | Any value but empty or `0` logs which native driver each lane uses and each driver-probe verdict with its duration |
| `TAPWING_NATIVE_RESTART_BUDGET_MS` | twice the restart timeout | How long all Android driver restarts of one run may take together (ms) |

In JSON reports (`--report`), each flow records its `lane` (`flutter`, `native`, or `hybrid`), per-lane command counts (`laneStats`), `blackbox: true` when no in-app driver connected (with `blackboxReason` when Tapwing knew before waiting), and `coldStartRetries` if any occurred.

*Note:* `--native-driver` and `tapwing.nativeDriver` in `config.yaml` are deprecated no-ops. Tapwing always uses its own native drivers.

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

**Apps without `tapwing_driver`.** If `launchApp` receives no driver handshake within 15 seconds (or a flow attaches to an uninstrumented app), Tapwing marks that `appId` as black-box for the flow and drives it through the native lane. Two misses in a row in one run make that last for the rest of the run. Most of the time Tapwing knows earlier than that, and does not wait at all (next section). A build that provably contains the driver is never driven black-box and never demoted, see [A build with the driver that does not connect](#a-build-with-the-driver-that-does-not-connect).

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

## A build with the driver that does not connect

The same probe also tells when the installed build does contain a driver that dials the host: it found `tapwing_driver_hello_v1`, `tapwing_port` or `TAPWING_PORT`. For such a build a missed handshake cannot mean "no driver". It is a slow start or a fault, so in `--input auto` Tapwing treats it differently from the builds above:

- **No black-box fallback and no demotion.** The app is never driven through the native lane in place of the driver, for the flow or for the rest of the run. Every later `launchApp` tries the driver again.
- **A slow start gets more time.** When the usual 15 seconds pass without a handshake and the app process is not known to be dead, the wait goes on until 60 seconds after the launch. Tapwing says so on stderr when the longer wait starts and checks once per second that the process is alive. A handshake in that time continues the launch on the Flutter lane.
- **A dead process ends the wait at once.** The step fails as an app crash, with the crash log excerpt under the step as for any other crash.
- **When the 60 seconds run out, `launchApp` fails.** The error names the evidence, the process and the deadline, and points to [troubleshooting.md](troubleshooting.md#app-fails-to-connect-to-tapwing_driver). The flow fails at that step and the next flow starts with a fresh launch.
- **The long wait is paid once.** After it ran out for an app, later launches of that app on the same device wait 15 seconds and their error says that the 60-second wait was not repeated. The next handshake restores it. A setup that cannot connect, such as a dead `adb reverse` tunnel, costs 60 seconds once and 15 seconds per flow after that.

```text
warning: launchApp com.example.app: no tapwing_driver handshake after 15 s, but the installed build contains the driver (found TAPWING_PORT, tapwing_driver_hello_v1, tapwing_port in base.apk: lib/arm64-v8a/libapp.so) and the app process 4321 is running; waiting up to 45 s more for a slow start (60 s in total), with no black-box fallback
```

```text
launchApp com.example.app: the installed build contains tapwing_driver (found TAPWING_PORT, tapwing_driver_hello_v1, tapwing_port in base.apk: lib/arm64-v8a/libapp.so) but the driver never connected: no handshake within 60 s, the app process 4321 is running. The app is not driven black-box, because a build with the driver belongs on the Flutter lane (pass --input native to drive it black-box on purpose). See "App Fails to Connect to tapwing_driver" in docs/troubleshooting.md
```

**Why it fails and does not fall back.** A flow written for the Flutter lane that runs through UiAutomator or XCUITest fails later at a lookup, and that failure names the wrong cause. On 2026-11-28, on an Android 15 emulator under a host load average near 300, two launches that took longer than 15 seconds demoted the app and five later flows of a 12-flow run failed in native mode. The same run passed 12 of 12 under lower load. Driven black-box, a build with the driver also loses what only the driver reports (app health issues and the HTTP log of `--trace`), and it loses them without a failure. A failed `launchApp` with the reason is the result a reader can act on. This is a change for a setup that cannot connect at all: simple flows used to pass black-box there and now fail at `launchApp`. `--input native` still drives any build black-box when that is what you want.

**Why 60 seconds.** It is the time in which the driver dials the host every 150 ms. After a minute without a host it slows to one dial per second. It is also four times the usual wait and as long as the longest startup Tapwing already waits for (the iOS XCUITest runner).

**What stays as it was.** A healthy launch is not touched: the first 15 seconds are the same wait as before, with no extra device call. A build the probe could not read (verdict unknown) and a build that links the driver without its dial path (a release build without `TAPWING_ALLOW_RELEASE`) behave as before: the flow goes black-box after 15 seconds, two misses in a row demote the app for the rest of the run, and the misses are remembered in `~/.tapwing/driver-probe.json`. `--input flutter` waits 20 seconds and fails, as before.

**In the reports.** A launch that connected during the longer wait has a `warning` on its `launchApp` step, a `driverProbe` object with `verdict: "present"`, `source`, `evidence`, `handshakeWaitedMs` and `slowStartWait: "used"`, and counts in the flow's `coldStartRetries`. A launch that failed has the error as the step's and the flow's failure message in run.json, the trace, the HTML report and the JUnit `<failure>`, and the same `driverProbe` object under the failed step's `driver` with `connected: false`, `processAlive` (`null` when the check could not tell) and `slowStartWait` (`"notRepeated"` for a launch that waited 15 seconds only). The flow is not marked `blackbox`, because it was not driven black-box.

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

**Slow cold starts.** On a busy machine, a freshly cleared app can take a while to come up. If the app has not connected 15 seconds after `launchApp` and the installed build provably contains the driver, the wait goes on until 60 seconds after the launch (see [A build with the driver that does not connect](#a-build-with-the-driver-that-does-not-connect)). If the first handshake after `launchApp` gets no answer for 10 seconds, Tapwing reconnects and gives the same process one more chance (up to 20 seconds more). On Android in native or black-box mode, if the screen is still empty after launch, the first required lookup gets an extra 10-second window (disabled under `--maestro-compat strict`).

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

In JSON reports (`--report`), each flow records its `lane` (`flutter`, `native`, or `hybrid`), per-lane command counts (`laneStats`), `blackbox: true` when the app was driven without the in-app driver (with `blackboxReason` when Tapwing knew before waiting), and `coldStartRetries` if any occurred (a handshake that was sent again, a longer wait for a slow start, or an extra window for the first native lookup).

*Note:* `--native-driver` and `tapwing.nativeDriver` in `config.yaml` are deprecated no-ops. Tapwing always uses its own native drivers.

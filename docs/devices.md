# Devices

Tapwing runs on Android emulators, physical Android devices (via `adb`), and the iOS Simulator (via `simctl`). Physical iOS devices are not supported.

## Choosing a device

If only one Android device or booted iOS Simulator is available, Tapwing picks it automatically. When several are running, it lists them and asks you to pass `--device` (or `--udid`):

```sh
tapwing test .maestro --device emulator-5554
tapwing test .maestro --device "iPhone 16 Pro"
tapwing test .maestro --device 3A4B5C6D-1234-5678-90AB-CDEF12345678
```

If `--device` matches a booted simulator's name or UDID, Tapwing uses iOS; otherwise it treats it as an `adb` serial. You can force the backend with `-p android` or `-p ios`.

To run tests across multiple devices in parallel, pass a comma-separated list like `--device "emulator-5554,emulator-5556"` (see [Workspaces, sharding and reports](workspaces-and-reports.md)).

## iOS Simulator

The iOS Simulator has no profile build mode, so build your app in debug mode with the Tapwing entrypoint:

```sh
flutter build ios --simulator --debug -t lib/main_tapwing.dart
xcrun simctl install booted build/ios/iphonesimulator/Runner.app
```

Then run flows or inspect the UI tree as usual:

```sh
tapwing test .maestro/login.yaml --device "iPhone 16 Pro"
tapwing hierarchy --device "iPhone 16 Pro" --app-id com.example.myapp
```

A few simulator details worth knowing:

- **No port forwarding needed.** The simulator shares your Mac's loopback interface, so the CLI passes the session port through the app's temporary container directory and only accepts connections from the launched PID.
- **State and permissions.** `clearState` empties the app's data container (a reinstall like Maestro's under `--maestro-compat strict`; what differs is in [Intentional differences](compatibility.md#intentional-differences)), and permissions go through `simctl privacy`.
- **Hardware keyboard.** Tapwing reads the simulator's hardware-keyboard setting so it never stalls waiting for a soft keyboard that won't appear.
- **Strict Maestro mode.** By default, `hideKeyboard` dismisses the keyboard with a warning even if a Flutter text field stays focused. Pass `--maestro-compat strict` if you want to reproduce Maestro's iOS failure there, and to make `clearState` reinstall the app like Maestro.

## What Tapwing does during a run

- **Isolated port per session.** `--port 0` (the default) picks a free host port for each run and removes `adb reverse` when done, so another app left running won't connect to the wrong session.
- **Keeps the screen awake.** On Android, Tapwing enables stay-awake while plugged in (`stay_on_while_plugged_in`) and restores your previous setting on exit. A sleeping screen is woken up; a PIN-locked device fails immediately.
- **Works without `launchApp`.** If a flow doesn't start with `launchApp`, Tapwing attaches to the app that is already running.
- **Never waits forever on host tools.** Every `xcrun simctl`, `adb install` and `adb start-server` call has a deadline (15–60 s for queries, launches and `terminate`, 180 s for `simctl install`, 300 s for `adb install`). A wedged CoreSimulator service or adb daemon fails the step with an error such as ``` `xcrun simctl launch …` did not finish within 60 s and was killed ``` instead of hanging the run, and Ctrl-C stops a call in flight.
- **Cleans up after a hard-killed run.** If an earlier `tapwing` was killed with `SIGKILL` or crashed, its screen recordings (`/sdcard/tapwing_rec_<pid>_<n>.mp4`) can stay on the Android device and its `xcodebuild` runner can keep running on the Mac. The next session removes them, but only when the Tapwing process named in the file or scratch-directory name is no longer running, so a concurrent run on the same device is never touched. A failed cleanup prints a warning and the run continues.
- **Platform and app checks.** Connections from another platform are rejected (useful when an iOS Simulator is still running in the background while you test on Android). Passing `--app-id` to `tapwing hierarchy` pins it to that specific app's process.

## Troubleshooting

See [Troubleshooting](troubleshooting.md) for common fixes (`adb` 37.0.1 mDNS crashes, zone mismatch warnings, driver connection checks, and leftover simulator/emulator processes).

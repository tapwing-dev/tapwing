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
- **State and permissions.** `clearState` reinstalls the app (just like Maestro), and permissions go through `simctl privacy`.
- **Hardware keyboard.** Tapwing reads the simulator's hardware-keyboard setting so it never stalls waiting for a soft keyboard that won't appear.
- **Strict Maestro mode.** By default, `hideKeyboard` dismisses the keyboard with a warning even if a Flutter text field stays focused. Pass `--maestro-compat strict` if you want to reproduce Maestro's iOS failure there.

## What Tapwing does during a run

- **Isolated port per session.** `--port 0` (the default) picks a free host port for each run and removes `adb reverse` when done, so another app left running won't connect to the wrong session.
- **Keeps the screen awake.** On Android, Tapwing enables stay-awake while plugged in (`stay_on_while_plugged_in`) and restores your previous setting on exit. A sleeping screen is woken up; a PIN-locked device fails immediately.
- **Works without `launchApp`.** If a flow doesn't start with `launchApp`, Tapwing attaches to the app that is already running.
- **Never waits forever on host tools.** Every `xcrun simctl`, `adb install` and `adb start-server` call has a deadline (15–60 s for queries, launches and `terminate`, 180 s for `simctl install`, 300 s for `adb install`). A wedged CoreSimulator service or adb daemon fails the step with an error such as ``` `xcrun simctl launch …` did not finish within 60 s and was killed ``` instead of hanging the run, and Ctrl-C stops a call in flight.
- **Platform and app checks.** Connections from another platform are rejected (useful when an iOS Simulator is still running in the background while you test on Android). Passing `--app-id` to `tapwing hierarchy` pins it to that specific app's process.

## Troubleshooting

### Flows fail with `app closed the connection` or `app did not connect` (adb 37.0.1)

Android platform-tools 37.0.1 has a bug where mDNS auto-discovery crashes the `adb` server (`SIGABRT` in `adbmdns_bridge::zero_config`) when emulators advertise themselves, which drops every `adb reverse` tunnel.

Disable mDNS discovery and restart `adb`:

```sh
export ADB_MDNS=0
adb kill-server
```

Add `export ADB_MDNS=0` to your shell profile (and run `launchctl setenv ADB_MDNS 0` on macOS for GUI apps). USB devices and manual `adb connect` keep working normally.

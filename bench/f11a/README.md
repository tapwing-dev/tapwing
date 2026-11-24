# F11a: Tapwing's Android driver

Tools to inspect `drivers/android`, Tapwing's native-lane driver. F11a compared
it with Maestro 2.12.1's driver on an emulator and a Pixel 8a before Maestro's
driver was removed; those results are kept:
[bench/results/f11a/summary.md](../results/f11a/summary.md) (emulator) and
[bench/results/f11a/pixel-8a.md](../results/f11a/pixel-8a.md) (physical device).

| What | How |
|---|---|
| Hierarchy of one screen | `cargo run --release --example native_hierarchy_diff -- dump --serial <s> --out <dir> --label <name>`: several dumps through the driver, saves the first, records start and `viewHierarchy` timings and whether the screen held still (`stable`, `stable up to window order`, `unstable`) |
| Hierarchy of all screens | `bench/f11a/run_parity.sh <serial> [screen ...]`: sets up each screen (launcher, Settings, Chrome, notification shade, quick settings, sample app incl. soft keyboard and the permission dialog) and runs `dump` |
| Diff two saved dumps | `native_hierarchy_diff diff a.xml b.xml [--ignore bounds]` |
| Unicode input | `native_hierarchy_diff type --serial <s> --text "Příliš žluťoučký kůň"`: types into the focused field, reads it back, checks the default IME |
| Mock location | `native_hierarchy_diff location --serial <s>`: `setLocation` through the driver's mock providers (the physical-device path), read back from `dumpsys location` |
| Cold install | add `--fresh` to `dump`: uninstalls the driver first |

UiAutomator's `getWindowRoots` collects the window roots in a `HashSet`, so two
dumps of an unchanged screen can differ in window order only; `dump` reports that
as `stable up to window order`.

Prerequisites: the sample app (`examples/sample_app`, profile build of
`lib/main_tapwing.dart`) installed, and Tapwing's driver (a release bundle,
`$TAPWING_DRIVER_APK`, or `drivers/android/build/tapwing-driver.apk` from
`scripts/build-android-driver.sh`). `chrome_web_input` and
`flows/chrome_web_unicode.yaml` need Chrome's first-run and sign-in screens
dismissed once and `web/index.html` on the device's `127.0.0.1:8099`
(`run_parity.sh` serves it and sets up `adb reverse`).

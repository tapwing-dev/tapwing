# Browser login through the native lane: baseline

Date: 2026-11-28 · Fixture, flows and runner: [bench/web-login](../../web-login/README.md)

A login in a system browser window is 83 to 85 % of the flow's step time in Tapwing on both platforms: 18 to 22 s on Android and 38 to 43 s on the iOS Simulator. On Android Tapwing's native steps take about as long as Maestro's, and typing costs 231 to 235 ms per character against 285 to 299 ms in Maestro. On iOS Tapwing is 7 to 8 s slower than Maestro per flow, and two steps account for it: the tap on the consent alert (10.6 s) and the first lookup in the sign-in sheet (17.5 s in every run). Nothing was optimised for this baseline.

## Environment

- Tapwing 0.1.1 at `0930946` plus the fixture commits of this branch (nothing changed under `crates/`, `packages/tapwing_driver` or `drivers/`), `cargo build --release`, both native drivers built from the checkout; Maestro 2.12.1; Flutter 3.47.5; Xcode 27.2
- Android: AVD `Pixel_8_Pro_API_35` as `emulator-5558` (Android 15, arm64, 1344x2992, en-US), Chrome 143.0.7499.192, Gboard, profile APK of `lib/main_tapwing.dart`. The browser is a regular Chrome Custom Tab (`preferAuthTabs: false`), not a Chrome Auth Tab
- iOS: iPhone 18 Pro simulator (iOS 27.2, en-GB), debug build of `lib/main_tapwing.dart`, default `ASWebAuthenticationSession` with its consent alert
- Host: Apple M1 Pro, 8 cores, 32 GB, shared with other work (see Caveats)
- Tapwing with `--input auto --report`. Maestro against the same driver build. No retries
- 1 discarded warm-up, then 5 runs per page variant and runner, interleaved run by run. Medians over the 5 runs. All 60 measured runs passed
- Account typed by the flows: a 20-character email and a 14-character password with upper case, digits and two symbols
- iOS only: the runner terminated `com.apple.SafariViewService` before every run (finding 4)
- Lanes come from the JSON report. It has none for `assertVisible`, so lanes marked `*` follow from the step's place in the flow. Separate `--trace` runs confirmed them, two on Android and one on iOS (finding 7)

## Key numbers

"Outside Flutter" is every step after the tap that opens the browser up to and including the tap on the submit button. T is Tapwing, M is Maestro.

### Android

| | `plain` T | `plain` M | `two-step` T | `two-step` M | `controlled` T | `controlled` M |
|---|---:|---:|---:|---:|---:|---:|
| Measured runs passed | 5/5 | 5/5 | 5/5 | 5/5 | 5/5 | 5/5 |
| Wall clock with CLI start | 23.20 s | 37.60 s | 26.30 s | 42.20 s | 23.60 s | 39.10 s |
| Whole flow, sum of steps | 22.14 s | 30.68 s | 25.43 s | 35.24 s | 22.65 s | 31.81 s |
| Outside Flutter, time | 18.29 s | 20.30 s | 21.75 s | 24.19 s | 18.86 s | 21.04 s |
| Outside Flutter, share of the flow | 83 % | 66 % | 85 % | 69 % | 84 % | 66 % |
| Outside Flutter, share without `launchApp` | 95 % | 69 % | 95 % | 72 % | 95 % | 69 % |
| `inputText` in the browser, per character (34 characters) | 231 ms | 299 ms | 233 ms | 285 ms | 235 ms | 295 ms |
| `tapOn` and `assertVisible` in the browser, sum | 10.80 s (4 steps) | 10.32 s | 13.90 s (6 steps) | 14.22 s | 10.83 s (6 steps) | 11.01 s |
| Submit tap until the first Flutter step passed | 3.76 s | 4.20 s | 3.70 s | 4.29 s | 3.51 s | 4.36 s |
| Host load average during the runs | 16 | 16 | 25 | 24 | 43 | 42 |

Tapwing, single steps: a native `tapOn` in the page takes 1.9 to 3.7 s, a native `assertVisible` on a loaded page 27 to 121 ms, the first `assertVisible` after the tab opens 2.0 to 2.2 s. The email (20 characters) types at 239 to 247 ms per character, the password (14 characters) at 217 to 223 ms.

### iOS Simulator

| | `plain` T | `plain` M | `two-step` T | `two-step` M | `controlled` T | `controlled` M |
|---|---:|---:|---:|---:|---:|---:|
| Measured runs passed | 5/5 | 5/5 | 5/5 | 5/5 | 5/5 | 5/5 |
| Wall clock with CLI start | 46.30 s | 39.30 s | 51.10 s | 43.20 s | 48.70 s | 40.80 s |
| Whole flow, sum of steps | 45.85 s | 25.76 s | 50.66 s | 29.05 s | 48.23 s | 26.74 s |
| Outside Flutter, time | 38.13 s | 16.44 s | 42.86 s | 19.50 s | 40.46 s | 17.16 s |
| Outside Flutter, share of the flow | 83 % | 64 % | 85 % | 67 % | 84 % | 64 % |
| Outside Flutter, share without `launchApp` | 91 % | 70 % | 92 % | 73 % | 92 % | 71 % |
| `inputText` in the browser, per character (34 characters) | 90 ms | 108 ms | 90 ms | 108 ms | 91 ms | 109 ms |
| `tapOn` and `assertVisible` in the browser, sum | 35.05 s (5 steps) | 12.74 s | 39.77 s (7 steps) | 15.84 s | 37.36 s (7 steps) | 13.47 s |
| Submit tap until the first Flutter step passed | 2.65 s | 2.65 s | 2.93 s | 2.69 s | 3.23 s | 2.44 s |
| Host load average during the runs | 85 | 88 | 75 | 79 | 71 | 73 |

Tapwing, single steps: a `tapOn` in the sheet takes 1.6 to 3.3 s, an `assertVisible` in the sheet 0.6 to 0.9 s, the first one 17.5 s. The email types at 82 ms per character, the password at 102 to 104 ms. Maestro's wall clock holds about 14 s of runner start that its step times do not show.

## Per step, Android (medians, ms)

`plain`

| # | step | Tapwing lane | Tapwing ms | Maestro ms |
|---|---|---|---:|---:|
| 1 | `launchApp (clearState)` | host | 2847 | 1323 |
| 2 | `tapOn id: browser_login_button` | flutter | 796 | 4963 |
| 3 | `tapOn id: browser_login_plain` | flutter | 147 | 4210 |
| 4 | `assertVisible "Tapwing Bench sign-in" is visible` (browser) | native* | 2019 | 74 |
| 5 | `tapOn "Email address"` (browser) | native | 2966 | 3373 |
| 6 | `inputText "qa.pilot@tapwing.dev"` (browser) | native | 4776 | 5832 |
| 7 | `tapOn "Password"` (browser) | native | 1920 | 2771 |
| 8 | `inputText "Tw!ng-Pass2026"` (browser) | native | 3083 | 3731 |
| 9 | `tapOn "Sign in"` (browser) | native | 3746 | 4125 |
| 10 | `assertVisible "Signed in as qa.pilot@tapwing.dev" is visible` | flutter* | 13 | 72 |

`two-step`

| # | step | Tapwing lane | Tapwing ms | Maestro ms |
|---|---|---|---:|---:|
| 1 | `launchApp (clearState)` | host | 2894 | 1301 |
| 2 | `tapOn id: browser_login_button` | flutter | 938 | 5061 |
| 3 | `tapOn id: browser_login_two_step` | flutter | 94 | 4366 |
| 4 | `assertVisible "Tapwing Bench sign-in" is visible` (browser) | native* | 2237 | 72 |
| 5 | `tapOn "Email address"` (browser) | native | 2891 | 3740 |
| 6 | `inputText "qa.pilot@tapwing.dev"` (browser) | native | 4789 | 5648 |
| 7 | `tapOn "Continue"` (browser) | native | 2351 | 2879 |
| 8 | `assertVisible "Account: qa.pilot@tapwing.dev" is visible` (browser) | native* | 27 | 74 |
| 9 | `tapOn "Password"` (browser) | native | 2493 | 3332 |
| 10 | `inputText "Tw!ng-Pass2026"` (browser) | native | 3121 | 3941 |
| 11 | `tapOn "Sign in"` (browser) | native | 3677 | 4216 |
| 12 | `assertVisible "Signed in as qa.pilot@tapwing.dev" is visible` | flutter* | 18 | 74 |

`controlled`

| # | step | Tapwing lane | Tapwing ms | Maestro ms |
|---|---|---|---:|---:|
| 1 | `launchApp (clearState)` | host | 2809 | 1329 |
| 2 | `tapOn id: browser_login_button` | flutter | 881 | 5002 |
| 3 | `tapOn id: browser_login_controlled` | flutter | 76 | 4376 |
| 4 | `assertVisible "Tapwing Bench sign-in" is visible` (browser) | native* | 2109 | 92 |
| 5 | `tapOn "Email address"` (browser) | native | 3059 | 3605 |
| 6 | `inputText "qa.pilot@tapwing.dev"` (browser) | native | 4950 | 6177 |
| 7 | `tapOn "Password"` (browser) | native | 2002 | 2642 |
| 8 | `inputText "Tw!ng-Pass2026"` (browser) | native | 3032 | 3886 |
| 9 | `assertVisible "Events: keydown [0-9]+, input [1-9][0-9]*, change [0-9]+" is visible` (browser) | native* | 121 | 217 |
| 10 | `assertVisible "Sign in" is visible` (browser) | native* | 87 | 151 |
| 11 | `tapOn "Sign in"` (browser) | native | 3492 | 4286 |
| 12 | `assertVisible "Signed in as qa.pilot@tapwing.dev" is visible` | flutter* | 17 | 69 |

## Per step, iOS Simulator (medians, ms)

`plain`

| # | step | Tapwing lane | Tapwing ms | Maestro ms |
|---|---|---|---:|---:|
| 1 | `launchApp (clearState)` | host | 4149 | 2425 |
| 2 | `tapOn id: browser_login_button` | flutter | 582 | 5327 |
| 3 | `tapOn id: browser_login_plain` | flutter | 3030 | 1260 |
| 4 | `tapOn "Continue"` (browser) | native | 10628 | 5680 |
| 5 | `assertVisible "Tapwing Bench sign-in" is visible` (browser) | native* | 17539 | 1229 |
| 6 | `tapOn "Email address"` (browser) | native | 1659 | 1751 |
| 7 | `inputText "qa.pilot@tapwing.dev"` (browser) | native | 1644 | 1985 |
| 8 | `tapOn "Password"` (browser) | native | 2616 | 1804 |
| 9 | `inputText "Tw!ng-Pass2026"` (browser) | native | 1426 | 1692 |
| 10 | `tapOn "Sign in"` (browser) | native | 2650 | 2296 |
| 11 | `assertVisible "Signed in as qa.pilot@tapwing.dev" is visible` | flutter* | 1.9 | 355 |

`two-step`

| # | step | Tapwing lane | Tapwing ms | Maestro ms |
|---|---|---|---:|---:|
| 1 | `launchApp (clearState)` | host | 4229 | 2465 |
| 2 | `tapOn id: browser_login_button` | flutter | 580 | 5446 |
| 3 | `tapOn id: browser_login_two_step` | flutter | 3032 | 1309 |
| 4 | `tapOn "Continue"` (browser) | native | 10634 | 5758 |
| 5 | `assertVisible "Tapwing Bench sign-in" is visible` (browser) | native* | 17544 | 1023 |
| 6 | `tapOn "Email address"` (browser) | native | 1634 | 1745 |
| 7 | `inputText "qa.pilot@tapwing.dev"` (browser) | native | 1643 | 1977 |
| 8 | `tapOn "Continue"` (browser) | native | 3325 | 2717 |
| 9 | `assertVisible "Account: qa.pilot@tapwing.dev" is visible` (browser) | native* | 611 | 233 |
| 10 | `tapOn "Password"` (browser) | native | 3128 | 1900 |
| 11 | `inputText "Tw!ng-Pass2026"` (browser) | native | 1432 | 1689 |
| 12 | `tapOn "Sign in"` (browser) | native | 2929 | 2325 |
| 13 | `assertVisible "Signed in as qa.pilot@tapwing.dev" is visible` | flutter* | 1.3 | 360 |

`controlled`

| # | step | Tapwing lane | Tapwing ms | Maestro ms |
|---|---|---|---:|---:|
| 1 | `launchApp (clearState)` | host | 4184 | 2484 |
| 2 | `tapOn id: browser_login_button` | flutter | 583 | 5484 |
| 3 | `tapOn id: browser_login_controlled` | flutter | 3030 | 1286 |
| 4 | `tapOn "Continue"` (browser) | native | 10553 | 5596 |
| 5 | `assertVisible "Tapwing Bench sign-in" is visible` (browser) | native* | 17536 | 1491 |
| 6 | `tapOn "Email address"` (browser) | native | 1703 | 1748 |
| 7 | `inputText "qa.pilot@tapwing.dev"` (browser) | native | 1644 | 1979 |
| 8 | `tapOn "Password"` (browser) | native | 2698 | 1833 |
| 9 | `inputText "Tw!ng-Pass2026"` (browser) | native | 1463 | 1708 |
| 10 | `assertVisible "Events: keydown [0-9]+, input [1-9][0-9]*, change [0-9]+" is visible` (browser) | native* | 860 | 409 |
| 11 | `assertVisible "Sign in" is visible` (browser) | native* | 791 | 432 |
| 12 | `tapOn "Sign in"` (browser) | native | 3230 | 2120 |
| 13 | `assertVisible "Signed in as qa.pilot@tapwing.dev" is visible` | flutter* | 1.4 | 313 |

## Slowest browser steps in Tapwing

| Platform | Step | Lane | Median |
|---|---|---|---:|
| Android | `inputText` email, 20 characters | native | 4.8 to 5.0 s |
| Android | `tapOn "Sign in"` | native | 3.5 to 3.7 s |
| Android | `inputText` password, 14 characters | native | 3.0 to 3.1 s |
| Android | `tapOn "Email address"` | native | 2.9 to 3.1 s |
| iOS | first `assertVisible` in the sheet | hybrid in the trace | 17.5 s |
| iOS | `tapOn "Continue"` on the consent alert | native, hybrid in the trace | 10.6 s |
| iOS | `tapOn "Continue"` and `tapOn "Sign in"` in the page | native, hybrid in the trace | 2.7 to 3.3 s |
| iOS | `tapOn "Password"` | native, hybrid in the trace | 2.6 to 3.1 s |

## Findings

1. **Android, the first lookup in the Custom Tab sometimes waits out the whole 17 s lookup timeout and then passes.** 1 of 15 measured Tapwing runs (`plain` run 5: 17,087 ms against a median of 2,019 ms), 1 of 3 warm-ups (17,061 ms) and 2 of 4 single runs outside the measurement (17,120 and 17,055 ms). The run still passes, so the median hides it. Reading `route_selector_opt` in `crates/tapwing/src/run/hybrid.rs` suggests the cause, which no experiment here confirmed: after a 400 ms Flutter probe Tapwing asks once which window has focus, and when Chrome does not have it yet (`mCurrentFocus=null` was seen right after the tap) it waits for the rest of the timeout on Flutter before it looks natively.
2. **iOS, the first lookup in the sign-in sheet takes the whole lookup timeout in every run.** 17.5 s in 18 of 18 Tapwing runs (15 measured, 3 warm-ups), against 1.0 to 1.5 s in Maestro. XCTest reports the app itself as the foreground app under the sheet, so Tapwing waits on Flutter first. The trace labels every later lookup in the sheet `hybrid` as well, at 0.6 to 0.9 s per `assertVisible`.
3. **iOS, the tap on the consent alert carries the cold start of the XCUITest runner.** 10.6 s against 5.7 s in Maestro, whose runner is up before the flow starts. Earlier the same day, with the host's load average at about 400 or more, the start did not fit into the step's 17 s timeout and `tapOn "Continue"` failed in 3 of 3 single runs with `Element not found` while the alert was on screen. A diagnostic copy of the flow with an early `setOrientation`, which starts the runner at flow start, tapped the alert in 2.8 s.
4. **iOS 27.2 Simulator, `com.apple.SafariViewService` stops answering accessibility requests, for both runners.** Without the per-run restart the warm-ups of both runners passed, then Tapwing failed 3 of 3 and Maestro 2 of 2 runs at `assertVisible "Tapwing Bench sign-in"` with the page on screen, until the run was stopped. The same process had served four sign-in sheets by then. Maestro's XCTest log shows `Failed to fetch Safari WebView hierarchy: Error getting main window kAXErrorServerNotFound` for that process. A process that had been running for 10 days failed the same way on the first attempt, and Tapwing's native hierarchy then held only the app root and the status bar. `xcrun simctl terminate <udid> com.apple.SafariViewService` fixes it each time.
5. **Android, a second UiAutomation client blocks the native lane.** While a device-control tool kept its accessibility-snapshot instrumentation running on the emulator, all three variants failed at the first browser step with `the native lane is unavailable: starting the Tapwing Android driver: the driver did not start in time` (3 of 3). Closing that tool's session fixed it. The message does not name the cause.
6. **Maestro 2.12.1 found no Android device.** `maestro --device emulator-5558 test` answered `Device emulator-5558 was requested, but it is not connected` and `maestro list-devices` printed no Android section while three emulators were attached, one of them `unauthorized`. Maestro ran through `--host localhost --port 5559`, a direct connection to the emulator's adb port.
7. **The JSON report has no lane for `assertVisible`.** `driver` is `null` for it in both lanes. `--trace` has a lane for every step: on Android `host` for `launchApp`, `hybrid` for the first lookup in the tab, `native` for the rest of the browser steps, `flutter` for the final assertion. On iOS every lookup in the sheet is `hybrid` and only `inputText` is `native`.
8. **Per-key typing reaches the JavaScript-controlled form.** On both platforms and with both runners the `controlled` page counted 34 `keydown`, 34 `input` and 2 `change` events for the 34 typed characters, in all 12 runs per platform. The server rejected no login.
9. **iOS, the Flutter tap that opens the sheet takes 3.03 s in Tapwing in every run** (3,027 to 3,037 ms in 18 runs), against 0.03 to 0.25 s for the same tap on Android.

## Facts for a web lane

- Android: while the Custom Tab is open, `adb shell cat /proc/net/unix` lists `@chrome_devtools_remote`. Through a temporary `adb forward` to that socket, `/json/version` answered `Chrome/143.0.7499.192` and `/json/list` listed the login page as a `page` target with a `webSocketDebuggerUrl`. The socket is still listed after the tab closes, as long as Chrome's process lives.
- iOS: the session's web content runs in `com.apple.SafariViewService` (`launchctl list` in the simulator shows `UIKitApplication:com.apple.SafariViewService`, and XCTest names that process id for the sheet's remote element). Nothing else was checked on iOS.

## Caveats

- Other agents built apps and ran a second simulator and a second emulator on the same Mac during the measurement. The load average was 14 to 44 during the Android runs and 68 to 90 during the iOS runs on 8 cores, and the Mac was swapping. Absolute times are higher than on an idle machine by an unknown amount. The runners were interleaved, so both saw the same load.
- The Android and iOS numbers were taken under different load and are not comparable with each other.
- The runners split time between steps differently. Maestro spends 4.2 to 4.4 s (medians) inside the Flutter tap that opens the Custom Tab, Tapwing returns from that tap in about 0.1 s and waits in the next step. Maestro's "outside Flutter" rows therefore miss the time the browser takes to open. Compare the whole-flow sums across runners.
- One sample app, one emulator and one simulator, no physical device.
- The login page is local: no network latency, no redirects through an identity provider, no bot detection, no password-manager prompt appeared.
- The iOS numbers include a fresh `SafariViewService` process in every run.
- Chrome's first-run and sign-in screens were not exercised. Chrome on the emulator was already set up and holds a Google account, so its data was left alone.
- Raw per-run logs and reports are not committed. This file is the record of the run.

## Not measured

- Typing by any other method (set-text, paste, CDP). This is the per-key baseline only.
- A real identity provider such as Auth0, and a page behind a network.
- Physical devices, other Android or iOS versions, other Chrome versions, a Chrome Auth Tab, an ephemeral iOS session without the consent alert.
- A localised consent alert. The iOS flows tap the English `Continue`.
- Tapwing with `--input native`, and Maestro on a build without the driver.
- How long the Android native driver and the iOS runner take to start on an idle host.

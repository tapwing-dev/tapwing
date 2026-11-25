# Tapwing vs `agent-device test --maestro`

Date: 2026-11-25 · Tracking issue: [#20](https://github.com/tapwing-dev/tapwing/issues/20)

With `tapwing_driver` in the app, Tapwing is faster on every pure-Flutter flow both runners pass. agent-device is faster on the hybrid `f5_permission_dialog` flow and much faster on a build without the driver. agent-device did not finish `login_and_browse` in any variant.

## Environment

- Tapwing `8128498` (0.1.1, `cargo build --release`), agent-device 0.21.36, Maestro 2.12.1, Flutter 3.50.0
- Round 2 devices: Android 15 emulator `emulator-5556` (en-US), iPhone 18 Pro Max simulator (iOS 27.2, cs-CZ)
- Same driver build for both runners (Android profile, iOS simulator debug), default flags, no retries
- 1 discarded warm-up, then 5 interleaved runs per runner; medians; every listed run passed 5/5

## Flows both runners pass unmodified (driver build)

| Flow | Tapwing | agent-device | agent-device ÷ Tapwing |
|---|---:|---:|---:|
| Android `f7_app_health` | 2.41 s | 5.95 s | 2.47× |
| Android `f8_trace_network` | 2.38 s | 5.95 s | 2.50× |
| Android `f2_endless_animation` | 3.72 s | 4.79 s | 1.29× |
| Android `f5_permission_dialog` | 5.56 s | **4.40 s** | **0.79×** |
| iOS `f7_app_health` | 4.98 s | 9.24 s | 1.86× |
| iOS `f8_trace_network` | 4.99 s | 8.08 s | 1.62× |
| iOS `f2_endless_animation` | 6.07 s | 6.82 s | 1.12× |

## `login_and_browse`

agent-device rejects the unmodified flow (`assertVisible` with `checked: true`) and failed 0/5 on two trimmed copies. The first copy only drops `checked: true`; the second also switches `tapOn "Email"` to `id: email_input` and drops the iOS `hideKeyboard`. On Android agent-device cannot find fields by label (`tapOn "Email"`, `tapOn "Name"`), on iOS it fails at `hideKeyboard` or `tapOn "Back"`. Tapwing passed the same copies 5/5.

Steps both runners complete on the second copy (sum of per-step medians):

| Platform | Steps | Tapwing | agent-device | Ratio |
|---|---|---:|---:|---:|
| Android | 1–21 | 6.18 s | 17.52 s | 2.84× |
| iOS | 1–18 | 10.99 s | 27.74 s | 2.52× |

## Coverage (13 flows per platform)

| | Tapwing passes | agent-device accepts | agent-device passes |
|---|---:|---:|---:|
| Android | 11 | 6 | 5 |
| iOS | 11 | 4 | 3 |

Tapwing's failures were environmental: a dark mode precondition, reference screenshots from a different screen, and English alert labels on a Czech simulator.

## Plain build, no driver (3 interleaved runs, all passed)

| Flow | Tapwing | agent-device |
|---|---:|---:|
| Android `f5_permission_dialog` | 18.49 s | 4.38 s |
| Android `f7_app_health` | 21.54 s (launch 15.3 s) | 5.73 s |
| iOS `f7_app_health` | 26.65 s (launch 19.0 s) | 8.49 s |

Tapwing waits its full 15 s driver handshake on every launch of an app without the driver.

## Where the time goes (per-step medians)

- `inputText`: agent-device about 2 s, Tapwing 0.05–0.2 s
- `scrollUntilVisible`: 7.7 s vs 1.9 s (Android), 15.8 s vs 5.8 s (iOS)
- Assertions: 30–1100 ms vs 1–10 ms
- App launch on iOS: Tapwing 3.1–3.6 s vs about 2.5 s
- `f5` step `tapOn "Request Location Permission"`: Tapwing 3.0 s vs 0.29 s, which accounts for the whole `f5` gap

## Maestro baseline (round 1, different devices)

Android `login_and_browse` on the plain build: 50.86 s and 54.40 s, 2 of 3 runs passed (one lost its adb connection). iOS: 0 of 3 passed on the Czech simulator. These runs used a different emulator and simulator than the tables above, so the numbers are not directly comparable.

## Caveats

- One sample app, emulator and simulator only
- The flows both runners pass are short (2–9 s)
- agent-device 0.21.36 was seven patch releases behind 0.21.43
- Raw per-run logs, reports and the runner scripts are not committed; this file is the record of the run

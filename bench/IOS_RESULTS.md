# iOS Simulator Phase: Differential Results (Tapwing vs Maestro 2.12.1)

Date: 2026-11-21 · iPhone 18 Pro simulator (iOS 27.2, 402×874 pt @3x, UDID `47862A4D-…`), Xcode 27.2 · Flutter 3.47.5 (**debug** builds: the simulator has no profile mode) · Maestro 2.12.1 · Tapwing iOS phase. Scope: simulator only, physical iOS devices are not supported.

Builds of `examples/sample_app` (bundle `dev.tapwing.sampleApp`):

| Build | Entry point | Used by |
|---|---|---|
| driver | `lib/main_tapwing.dart` | Tapwing |
| plain | `lib/main.dart` | Maestro |
| driver / plain "stock" | same, `--dart-define=SAMPLE_IOS_TAP_OUTSIDE_UNFOCUS=false` | `hideKeyboard` matrix only |

The sample app unfocuses text fields on taps outside them **on iOS only** (`EditableTextTapOutsideIntent` in `MaterialApp.builder`, gated on `defaultTargetPlatform == TargetPlatform.iOS`), so stock Maestro's iOS `hideKeyboard` works on it; Android behavior is unchanged. The simulator ran with the host keyboard disconnected (`ConnectHardwareKeyboard = 0`), i.e. with a real soft keyboard.

## Summary

| Check | Result |
|---|---|
| Hierarchy parity vs `maestro hierarchy` | **10/10 screens, 98/98 app elements identical** (attributes, exact point bounds, order); 0 mismatches |
| Benchmark flow (`.maestro/ios/login_and_browse.yaml`, 28 steps) | Tapwing **3/3**, Maestro **3/3**; Tapwing **0 settle violations** |
| Benchmark speed (medians) | wall clock **62.7 s → 15.2 s (4.1×)**, excluding `launchApp` **48.5 s → 12.2 s (4.0×)**, median per-command **10×** |
| Differential flows (`.maestro/ios/`) | **10/10 positive pass in both**, **2/2 negative fail in both** (`rc=1`) |
| `hideKeyboard` matrix | identical pass/fail to Maestro in strict mode; default mode passes with a warning exactly where Maestro fails |
| Android regression (`emulator-5554`, F2 flows) | 7/7 positive pass, 2/2 negative fail, 0 settle violations |

## Hierarchy Parity

`bench/ios/capture_hierarchies.sh <udid>` navigates to each screen with Tapwing, then dumps `maestro hierarchy` and `tapwing hierarchy` of the same state and compares them with `bench/tools/diff_hierarchy_ios.py`: only the application subtree of Maestro's dump (status bars excluded), system-keyboard subtrees pruned, every element carrying `accessibilityText`/`text`/`hintText`/`resource-id` compared as an ordered multiset on (accessibilityText, text, hintText, resource-id, bounds, checked, enabled, selected, focused).

| Screen | Elements | Result |
|---|---:|---|
| `01_login` | 8 | identical |
| `02_login_typed` (soft keyboard up) | 9 | identical |
| `03_products` | 14 | identical |
| `04_detail_favorite` (switch on: `text="1"`, `checked`) | 6 | identical |
| `05_dialog_typed` (soft keyboard up) | 6 | identical |
| `06_playground` | 8 | identical |
| `07_selectors_taps` (read-only long text field, square box) | 14 | identical |
| `08_swipe_scroll` | 20 | identical |
| `09_text_keys` | 8 | identical |
| `10_screenshot_theme` | 5 | identical |

This includes the application root element (`accessibilityText = "Sample App"`, `[0,0][402,874]`), multi-line merged labels (`"Product 1\n€ 3.00"`), text fields (`accessibilityText` = label, `text` = value, empty `hintText`), `focused = false` on a focused field, and the split of a `Semantics(identifier:)` wrapper from its labelled child. Raw dumps: `bench/results/ios-simulator/hierarchy/`.

## Benchmark

`bench/ios/run_ios_bench.sh 3 <udid>`: the F2 benchmark flow in its iOS variant (`hideKeyboard` kept; `back` kept as a Maestro iOS no-op followed by `tapOn: "Back"`). Medians of 3 runs, raw data and per-command table in `bench/results/ios-simulator/bench/`.

| | Maestro | Tapwing | Speedup |
|---|---:|---:|---:|
| Wall clock incl. CLI start | 62.7 s | 15.2 s | **4.1×** |
| Sum of commands | 50.5 s | 15.0 s | **3.4×** |
| Excluding `launchApp` | 48.5 s | 12.2 s | **4.0×** |
| Median per-command speedup (excl. `launchApp`) | | | **10×** |
| Settle violations (`--check-settle`) | | 0 / 0 / 0 | |

Notable steps (Maestro → Tapwing): `hideKeyboard` 5.3 s → 0.12 s, `assertNotVisible` 0.69 s → 0.9 ms, `scrollUntilVisible "Product 40.*"` 19.7 s → 6.9 s. Where the gap is smaller, the time is spent by the app or iOS, not the runner:

- `launchApp (clearState)` 2.0 s → 3.0 s: Tapwing reinstalls the 139 MB debug bundle (~0.85 s, like Maestro's `clearAppState`), applies `simctl privacy` (~0.3 s), launches (~0.33 s) and waits for the debug-mode app to start and dial in (~1.1–1.3 s). Maestro's `launchApp` returns without waiting for the app to start; that cost shows up in its next step (`assertVisible "Welcome back"`, 3.5 s vs 6 ms).
- `tapOn "Email"` / `tapOn password_input` (~0.8 s each): the real iOS soft keyboard slides in (~0.7 s); Tapwing waits for it to settle instead of racing it.
- `assertVisible "Products"` (0.5 s): the sample app's simulated 600 ms login request.
- `scrollUntilVisible` (6.9 s): Maestro-equivalent fling velocity with iOS `BouncingScrollPhysics` coasting, kept on purpose (see F2 review: removing fling changes pass/fail for snapping scrollables).

Compared with the spike (F2.1 driver, Android CLI semantics), the iOS flow now runs with iOS semantics end to end and includes `hideKeyboard`; Maestro passed 3/3 here (the spike's 2/3 `scrollUntilVisible` failures did not reproduce).

## Differential Flows

`bench/ios/run_ios_differential.sh <udid>`: every flow in `examples/sample_app/.maestro/ios/` with Tapwing (`--check-settle`) and Maestro. Raw data: `bench/results/ios-simulator/differential/`.

| Flow | Tapwing | Maestro | Tapwing wall | Maestro wall | Notes |
|---|---|---|---:|---:|---|
| `login_and_browse.yaml` | PASS | PASS | 15.6 s | 63.5 s | `back` no-op parity (`Details of Product 40` still visible), `hideKeyboard` |
| `f2_selectors_and_taps.yaml` | PASS | PASS | 26.1 s | 66.5 s | Element-relative `point: "20, 20"` in points, `traits`, retry/`waitUntilVisible`, 8.5 s `assertNotVisible` |
| `f2_swipe_and_scroll.yaml` | PASS | PASS | 22.5 s | 78.7 s | Absolute coordinates in points (`200, 600 → 200, 350`), iOS `swipe: UP` from 90 %, `PageView`, `Dismissible` |
| `f2_text_clipboard_keys.yaml` | PASS | PASS | 12.9 s | 65.6 s | `back`, `pressKey: Back/Tab/Escape` are no-ops on both; `Backspace`/`Enter` typed into the app |
| `f2_screenshots_and_theme.yaml` | PASS | PASS | 6.5 s | 21.7 s | `assertScreenshot cropOn` against a Maestro-captured @3x reference (`ref_visual_badge_ios.png`, 600×264); Android reference only warns (size mismatch) |
| `f2_dark_mode.yaml` | PASS | PASS | 5.4 s | 21.8 s | `setDarkMode`, `toggleDarkMode`, `assertDarkMode`, `assertLightMode` via `simctl ui appearance` |
| `f2_endless_animation.yaml` | PASS | PASS | 6.1 s | 37.2 s | |
| `f2_hide_keyboard.yaml` | PASS | PASS | 6.6 s | 41.9 s | Three `hideKeyboard`s |
| `f2_ios_device_commands.yaml` | PASS | PASS | 5.5 s | 18.8 s | `launchApp` `permissions`/`clearKeychain`, `setPermissions`, `setLocation`, `clearKeychain`, `setAirplaneMode` (ignored), `pressKey: Volume Up` (ignored), relaunch after `simctl privacy` killed the app |
| `negative/fail_missing_element.yaml` | FAIL `rc=1` | FAIL `rc=1` | 4.1 s | 17.3 s | |
| `negative/fail_assert_not_visible.yaml` | FAIL `rc=1` | FAIL `rc=1` | 4.3 s | 17.7 s | |

Settle violations: 0 in every flow except one step of `f2_swipe_and_scroll.yaml` (`scrollUntilVisible "HCard 10"`), which sets `waitToSettleTimeoutMs: 300`; iOS bouncing physics keeps coasting past that explicit 300 ms cap, so the step returns while the list still moves, exactly as the flow requests. The following step passes.

Findings fixed during the run:
- Maestro rejects an explicit `location: allow` permission (only `always|inuse|never|unset`; `allow` maps to `always` only through `all`). Tapwing accepted it; it now fails with Maestro's message.
- `simctl privacy` terminates a running app whose permissions change. `launchApp: stopApp: false` now detects the new PID reported by `simctl launch` and reconnects, and `setPermissions` drops the old connection.

## `hideKeyboard` Matrix

`bench/ios/hide_keyboard_focus.yaml`: focus a field on "Text & Keys" (nothing under the screen center, where Maestro's micro-swipes start), type, `hideKeyboard`. Raw data: `bench/results/ios-simulator/hide-keyboard/`.

| App | Maestro 2.12.1 | Tapwing default | Tapwing `--maestro-compat strict` |
|---|---|---|---|
| stock (no tap-outside unfocus) | **FAIL** "Couldn't hide the keyboard…" | PASS + compatibility warning (keyboard hidden, focus kept, as on Maestro Android) | **FAIL** with Maestro's message |
| tap-outside unfocus (sample app default) | PASS | PASS (+ warning that the field kept focus) | PASS (micro-swipe unfocuses the field) |

Default mode never adds a failure Maestro would not have, and warns wherever Maestro iOS may fail; strict mode reproduces Maestro iOS's outcome. On the login screen the same flow also showed that the emulation is faithful in its side effects: Maestro's second micro-swipe (13 pt, below Flutter's 18 pt touch slop) is a *tap* at the screen center, which hits the "F2 Playground" button there, and both Maestro and Tapwing strict navigated away and failed the next step identically.

## Caveats

- Debug builds only (the simulator does not run Flutter profile/release builds), so absolute app-side times are slower than on Android profile builds.
- `pressKey: Home`/`Lock` are not supported on iOS yet: Maestro presses those hardware buttons through XCTest, which Tapwing has no lane for (native lane, F5). All other non-typing keys are ignored like in Maestro.
- `setPermissions` covers what `simctl privacy` can set (calendar, contacts, location, photos, media library, microphone, motion, reminders, Siri). Camera, Face ID, HomeKit, speech, notifications, and user tracking (set by Maestro through `applesimutils`) are reported in a warning and skipped.
- The hardware-keyboard hint is read from Simulator.app preferences (`DevicePreferences.<UDID>.ConnectHardwareKeyboard`, then the global `ConnectHardwareKeyboard`, default connected). These runs used a disconnected host keyboard; the connected case was not re-measured in this phase (the spike measured the wait it removes).
- `openLink` was verified manually (`simctl openurl`, then `launchApp: stopApp: false`), not in the differential suite, because it brings Safari to the foreground.
- System alerts, other apps, and keyboard elements remain outside the in-app driver (native lane, F5); the hierarchy diff prunes the keyboard for that reason.

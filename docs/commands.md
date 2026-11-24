# Supported commands

Tapwing supports the standard Maestro command set and selectors. If a flow works in Maestro, it should run the same way here.

## App lifecycle and device

| Commands | Notes |
|---|---|
| `launchApp` | Supports `clearState`, `clearKeychain`, `stopApp`, `permissions` (defaults to `all: allow`), and `launchArguments` (typed Android intent extras) |
| `stopApp`, `killApp` | Stops or kills the app process (`killApp` equals `stopApp` on iOS Simulator) |
| `clearState`, `clearKeychain` | On iOS Simulator, `clearState` reinstalls the app (matching Maestro) |
| `setPermissions` | Android via `pm`/`appops`; iOS Simulator via `simctl privacy` |
| `openLink` | Opens a URL or deep link (supports `browser` pinning on Android) |
| `setLocation`, `travel` | Emulator console or mock location provider on Android; `simctl location` on iOS |
| `setOrientation` | Android system rotation; iOS Simulator via XCUITest native lane |
| `setDarkMode`, `toggleDarkMode`, `assertDarkMode`, `assertLightMode` | Works even when no app is running |
| `setAirplaneMode`, `toggleAirplaneMode` | Supported on Android; no-op with a warning on iOS Simulator |
| `addMedia` | Pushes media files to the device library |
| `startRecording`, `stopRecording` | Records screen video to a file and cleans up safely if interrupted |
| `waitForAnimationToEnd` | Waits for Flutter UI idle, or visual stability in native lanes |

## Taps and assertions

- **`tapOn`, `longPressOn`, `doubleTapOn`:** target an element by selector, absolute coordinates, or element-relative `point`. Supports `repeat`, `delay`, `retryTapIfNoChange`, `waitUntilVisible`, and `waitToSettleTimeoutMs`.
- **`assertVisible`, `assertNotVisible`:** verify an element is present or absent.
- **`extendedWaitUntil`:** wait for `visible` or `notVisible` with a custom timeout.
- **`assertTrue`:** assert a JavaScript expression evaluates to true.

## Scrolling and swiping

- **`scroll`:** standard scroll gesture.
- **`swipe`:** directional across the screen, between coordinates or percentages, or starting `from:` an element (with optional relative `point` and `waitToSettleTimeoutMs`).
- **`scrollUntilVisible`:** supports all four directions, `speed`, `timeout`, `visibilityPercentage`, `centerElement`, and `waitToSettleTimeoutMs`.

## Text, clipboard and keys

- **`inputText` and `inputRandom*`:** supports `Text`, `Number`, `Email`, `PersonName`, `CityName`, `CountryName`, and `ColorName`.
- **`eraseText`:** succeeds with a warning if no field is focused (matching Maestro). Pass `--strict-input` to fail instead.
- **`hideKeyboard`:** on Android, falls back to a Back press with a warning if no keyboard is visible. On iOS, dismisses the keyboard with a warning if the field stays focused (pass `--maestro-compat strict` to match Maestro's failure there).
- **`copyTextFrom`, `pasteText`, `setClipboard`:** read and write clipboard text.
- **`back` and `pressKey`:** on Android, `back` / `pressKey: Back` sends a real system Back key so the keyboard, Flutter navigator, or Android predictive back handles it naturally. `Enter`, `Backspace`, `Tab`, `Escape`, and system keys (`Home`, `Lock`, etc.) are supported. On iOS Simulator, `Home` and `Lock` go through the native lane; `back`, `Tab`, `Escape`, and volume/remote keys are no-ops (matching Maestro).

## Screenshots

- **`takeScreenshot`:** writes `<name>.png` into `<test-output-dir>/screenshots/` (default `./.tapwing/<run>/screenshots/`).
- **`assertScreenshot`:** compares the screen (or a `cropOn` region) against a reference image searched in the flow directory and `--test-output-dir`, using Maestro's pixel-diff math. On mismatch, saves `<name>_diff.png` under `screenshot-diffs/`.

## Flow control and scripting

- **`runFlow`:** run another flow file or inline `commands`, with optional `when` condition and scoped `env`.
- **`repeat`:** repeat commands a fixed number of `times` or `while` a condition holds.
- **`retry`:** retry a block on command failures (`maxRetries` defaults to 1, capped at 3).
- **`runScript`, `evalScript`:** execute JavaScript with persistent `output`, `maestro`, `http`, `json()`, `relativePoint()`, `console.log`, and `faker`.
- **`onFlowStart`, `onFlowComplete`:** setup and teardown hooks (`onFlowComplete` runs even when the flow fails).
- **Variables:** set in flow `env:` or passed via `-e KEY=VALUE` (CLI `-e` takes precedence).

## Selectors

All Maestro selectors and relative matchers are supported:

- **Attributes:** `text`, `id`, `traits` (`text`, `long-text`, `square`), `css` (empty match on mobile, like Maestro), `index`, `enabled`, `checked`, `focused`, `selected`, `width`, `height`, `tolerance`.
- **Relative:** `below`, `above`, `leftOf`, `rightOf`, `childOf`, `containsChild`, `containsDescendants`.

## Timeouts, settle detection and `optional`

- **Timeouts:** `assertVisible` and `assertNotVisible` default to 17 s; optional lookups (`tapOn`, `copyTextFrom`, `swipe from`) default to 7 s. Both adjust for time already spent since the last mutating command, matching Maestro.
- **Settle detection:** Tapwing waits for coasting scrolls to stop and ignores endless decorative animations (such as loading spinners running over 1.5 s without changing the UI tree) so flows don't hang.
- **`optional: true`:** ignores command-level failures (like a missing element). Invalid arguments and lost device connections still fail.

## iOS Simulator differences

Tapwing matches Maestro's iOS behavior on the simulator (see [bench/IOS_RESULTS.md](../bench/IOS_RESULTS.md)):

- **Coordinates are in logical points**, not physical pixels, matching `maestro hierarchy` and iOS flow coordinates.
- **Hierarchy attributes** match iOS conventions: `accessibilityText` is the label, `text` is the value (`"1"`/`"0"` for switches and checkboxes), `hintText` is empty, and the root element uses the app's display name.
- **`setPermissions`** uses `simctl privacy`, which cannot toggle camera, Face ID, HomeKit, speech, notifications, or user tracking. Tapwing reports those clearly when requested.
- **`setAirplaneMode`** is ignored with a warning, and **`back`** / `Tab` / `Escape` are no-ops, matching Maestro on iOS.

# Supported commands

Tapwing supports the standard Maestro command set and selectors. If a flow works in Maestro, it should run the same way here.

## App lifecycle and device

| Commands | Notes |
|---|---|
| `launchApp` | Supports `clearState`, `clearKeychain`, `stopApp`, `permissions` (defaults to `all: allow`), and `launchArguments` (typed Android intent extras) |
| `stopApp`, `killApp` | Stops or kills the app process (`killApp` equals `stopApp` on iOS Simulator) |
| `clearState`, `clearKeychain` | On iOS Simulator, `clearState` empties the app's data container instead of reinstalling the app; `--maestro-compat strict` reinstalls like Maestro (see [Intentional differences](compatibility.md#intentional-differences)) |
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

- **`runFlow`:** run another flow file or inline `commands`, with optional `when` condition and scoped `env`. Sub-flow files are expanded when the flow is parsed, so flows that run each other in a circle are rejected up front with `Sub-flow Cycle` and the chain (`a.yaml → b.yaml → a.yaml`), whatever their `when` says; nesting is limited to 64 levels.
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
- **Network and images:** a step also waits for the app's in-flight `dart:io` HTTP requests and pending images, see [Loads that keep a step waiting](#loads-that-keep-a-step-waiting).
- **`optional: true`:** ignores command-level failures (like a missing element). Invalid arguments and lost device connections still fail.

### Loads that keep a step waiting

After each action Tapwing waits (up to the step's settle timeout, 3 s by default) until the app has no HTTP request in flight and no image still loading, so the next step sees the loaded screen. Two options keep requests that never finish from slowing every step down:

| Flag | `config.yaml` (`tapwing:`) | Purpose |
|---|---|---|
| `--settle-load-timeout-ms <ms>` | `settleLoadTimeoutMs` | How long a single HTTP request, or a pending image, counts as busy, measured from when it started. Default `5000`; `0` waits until it ends, as Tapwing did before this option existed |
| `--settle-http-ignore <regex>` | `settleHttpIgnore` | Requests whose URL matches never count as busy (repeatable). For analytics, long-poll and streaming endpoints |

```yaml
tapwing:
  settleLoadTimeoutMs: 5000
  settleHttpIgnore:
    - "^https://analytics\\."
    - "/events/poll"
```

- The CLI timeout overrides `config.yaml`; `--settle-http-ignore` patterns from the CLI are added to the ones in `config.yaml`.
- Patterns are Dart regular expressions, matched anywhere in the full request URL (scheme, host, path and query). They are compiled in the app, so an invalid one fails `launchApp` with the driver's error.
- A request shorter than the bound is waited for exactly as before. The 5 s default is longer than the 3 s settle timeout, so no single step ends sooner than it used to: it either sees the request finish or reaches its settle timeout first. What changes is that the steps after it stop waiting for a request that is already more than 5 s old (a hung call, a download, a long-poll). A long-poll loop opens a new request each time and each one blocks for the bound again, so put such endpoints in `settleHttpIgnore`.
- If a later step needs the result of a slow request, wait for what it shows (`assertVisible`, `extendedWaitUntil`) rather than relying on settle; `waitForAnimationToEnd` is bounded the same way.
- Server-sent event streams (`text/event-stream`) and WebSocket connections stop counting as busy as soon as they are established. Requests past the bound and ignored requests still appear in the `--trace` request log.
- Both settle policies (`--settle-policy v2|legacy`) use the same rules; `--check-settle` only reports animations and is not affected.
- Needs a `tapwing_driver` that knows the options. An app built with an older driver ignores them (Tapwing prints a warning when you set one) and waits for every request.

## iOS Simulator differences

Tapwing matches Maestro's iOS behavior on the simulator (see [bench/IOS_RESULTS.md](../bench/IOS_RESULTS.md)):

- **Coordinates are in logical points**, not physical pixels, matching `maestro hierarchy` and iOS flow coordinates.
- **Hierarchy attributes** match iOS conventions: `accessibilityText` is the label, `text` is the value (`"1"`/`"0"` for switches and checkboxes), `hintText` is empty, and the root element uses the app's display name.
- **`setPermissions`** uses `simctl privacy`, which cannot toggle camera, Face ID, HomeKit, speech, notifications, or user tracking. Tapwing reports those clearly when requested.
- **`setAirplaneMode`** is ignored with a warning, and **`back`** / `Tab` / `Escape` are no-ops, matching Maestro on iOS.

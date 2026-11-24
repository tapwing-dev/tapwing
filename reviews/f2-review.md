# F2 Review (Tapwing): Speed and Parity with Maestro 2.12.1

Date: 2026-11-20 · Scope: uncommitted diff over `bfe8d6a` + `crates/tapwing/src/{random,screenshot}.rs`, `spikes/ios/` · References: Maestro 2.12.1 source (`4413a33`), Flutter SDK 3.47.5.

Read-only code review (no device, emulator, or simulator touched).

**Checks executed:**
- `cargo test`: 18/18 (13 unit + 2 compat + 3 yaml).
- `flutter test`: 45/45.
- `dart analyze`: clean.

**Baseline data at review time (`bench/results/emulator-5554/*`):**
- Sum excluding `launchApp`: **5,392 ms**
- `launchApp (clearState)`: 1,628–2,039 ms
- `scrollUntilVisible "Product 40.*"`: 1,995–2,011 ms, consistently 5 swipes

*(Note: all findings below were subsequently addressed in F2.1; see [bench/F0_RESULTS.md](../bench/F0_RESULTS.md#f21).)*

---

## 0. Executive Summary

1. **F2 is largely a faithful port.** Selectors (`Filters.kt`), relative points, `retryTapIfNoChange`, `waitUntilVisible`, swipe ratios, `speed`→duration, `centerElement`, `resolveText`, `ScreenshotMatch`, and keycodes match Maestro's source. Verified claims are listed in §2.
2. **Three items marked complete in F2 change pass/fail outcomes:**
   - `back` and `pressKey: Back` pop the route while the soft keyboard is open; Maestro dismisses the keyboard only (#1).
   - `assertNotVisible` times out at 7 s instead of Maestro's 17 s (#2).
   - `cropOn` crops a screenshot captured *before* waiting for the target element (#3).
3. **Scroll mechanics affect pass/fail.** The iOS spike claim ("does not affect outcome") does not hold for `PageView`, carousels, and other snapping physics:
   - A zero-inertia drag moves 40% of the screen and `PageScrollPhysics` snaps back to the original page, failing in Tapwing while passing in Maestro.
   - **Recommendation:** keep Maestro-equivalent fling velocity (via virtual timestamps) as the default; optimize around it (#4).
4. **The F0→F2 regression (+0.85 s) is the price of parity.**
   - `ClampingScrollSimulation` at 593 lp/s (1,248 px over 601 ms @ dpr 3.5) coasts ~312 ms and ~274 px. 5 swipes × (64 ms drag + 312 ms fling) ≈ 1.9 s (measured 2.0 s).
   - ~150–250 ms can be saved without changing semantics. Larger wins live elsewhere: real-time `swipe`/`scroll` (~300 ms/command), infinite large animations (3 s settle timeout), and absent soft keyboards (700 ms).
5. **4–6 s `launchApp` did not reproduce in the latest data** (1.6–2.0 s), though two hard 5 s caps (`hello` read and `waitIdle`) and missing PID verification are on the critical path (#8).
6. **Differential flows prove less than claimed** (#10):
   - Screenshot compared against itself
   - `traits: text` matches every node on Android
   - `Tab` focus move not actually asserted
   - Missing `PageView`, `back` with keyboard open, and negative cases
   - Maestro reference fixture test silently skipped when scratchpad path is absent

---

## 1. Ranked Findings

| # | Severity | Finding | Changes pass/fail? |
|---|---|---|---|
| 1 | Maestro divergence | `back` / `pressKey: Back` with soft keyboard open pops route instead of hiding keyboard; `hideKeyboard` with no keyboard behaves inversely | Yes (fix restores parity) |
| 2 | Maestro divergence | `assertNotVisible` 7 s vs 17 s, optional `assertVisible` 7 s vs 17 s, missing `adjustedToLatestInteraction` | Yes |
| 3 | Correctness bug | `takeScreenshot`/`assertScreenshot` with `cropOn` captures screenshot before locating the element | Yes |
| 4 | Speed + compat | Scroll strategy: keep fling default; accelerate `swipe`/`scroll` (currently real-time), shorten drag; hold-drag opt-in only | Fling: no; hold-drag: yes |
| 5 | Speed | Infinite significant animation (≥ 2% screen) hits 3 s settle timeout on every step; Maestro is unaffected | No |
| 6 | Correctness + speed | Spike fixes missing from F2: coasting scroll (swallowed taps) and adaptive keyboard waiter | 6a yes (fewer false fails); 6b no |
| 7 | Maestro divergence | `launchApp` lacks default `permissions: all: allow`; `killApp` uses `force-stop` instead of `am kill` | Yes |
| 8 | Speed + diagnostics | `launchApp`: 5 s caps on critical path, missing PID verification, 2–3 ADB spawns | No |
| 9 | Correctness (timeouts) | Rust read timeout shorter than worst-case handler; shared queue across reconnects in `RpcSession` | Yes (false failures) |
| 10 | Test gap | Differential flows and unit tests do not actually prove key claims | – |
| 11 | Maestro divergence | `assertScreenshot` searches CWD instead of artifacts dir; `takeScreenshot` does not unconditionally append `.png` | Yes |
| 12 | Maestro divergence | Unfocused `inputText`/`pasteText`/`inputRandom*` fail instead of silently dropping like Maestro | Yes |
| 13 | Speed | ADB `pressKey`: process spawn + fixed 300 ms sleep without settle | No |
| 14 | Speed | Screenshot: on-device PNG encode + host decode | No |
| 15 | Maestro divergence | `scrollUntilVisible`: missing 500 ms per-iteration lookup, extra final check | Minor |
| 16 | Maestro divergence | `assertDarkMode` reads `platformBrightness` instead of `cmd uimode night` and requires running app | Minor |
| 17 | Speed (small) | 3 full snapshots per `tapOn`; per-frame snapshot in `waitFor` without hash gate | No |
| 18 | Speed (small) | Sequential CLI setup/teardown (~50–75 ms per ADB spawn) | No |
| 19 | Cleanup | `KNode.error` is never populated for pure Flutter UI; clarify dead code and README | No |
| 20 | Cleanup | Virtual pointer clock accumulates monotonically; document in contract | No |
| 21 | Divergence (nit) | `optional` downgrades non-Maestro errors; `cropOn.optional` leaks; `swipe` precedence | Minor |
| 22 | Risk (plausible) | `retryTapIfNoChange` retries ~50 ms after first tap, inside `kDoubleTapTimeout` | Possibly |

---

### 1. `back` / `pressKey: Back` Pops Route While Keyboard Is Open

- **Severity:** Maestro divergence, changes pass/fail.
- **Location:** `command_handlers.dart:244-249, 282-283` → `pointer_input.dart:183-189` (always dispatches `popRoute`). Maestro `AndroidDriver.kt:483-496, 287-325` sends `input keyevent 4`, which Android delivers to the IME first (`ImeInputStage`).
- **Scenario:** `tapOn "Name"` → `back` → `assertVisible "New product"`. Maestro hides the keyboard and passes; Tapwing pops the dialog and fails. Inversely, `hideKeyboard` with no keyboard visible navigates back in Maestro, whereas Tapwing was a no-op.
- **Fix:** When `viewInsets.bottom > 0`, `back` and `pressKey BACK` call `hideKeyboard()` without `popRoute`. When `viewInsets.bottom == 0`, `hideKeyboard` sends `popRoute` with a compatibility warning.

### 2. Assertion Timeouts: `assertNotVisible` 7 s vs 17 s, Missing `adjustedToLatestInteraction`

- **Severity:** Maestro divergence, changes pass/fail.
- **Location:** `run.rs:609, 615`, `rpc_messages.dart:20`. In Maestro (`Orchestra.kt:560-561, 1116-1135, 1486-1495, 1830-1833`), both `assertVisible` and `assertNotVisible` (even when `optional: true`) default to 17 s; 7 s applies only to `when:` conditions and optional element lookups (`tapOn`, `copyTextFrom`, `swipe from`, `cropOn`). All lookup timeouts are shortened by elapsed time since the last mutating command (`adjustedToLatestInteraction`).
- **Fix:** Track `last_interaction: Instant` in `run_flow`, subtract elapsed time from all lookup/assertion timeouts, remove `notVisibleMs`, and update README.

### 3. `cropOn` Captures Screenshot Before Locating Element

- **Severity:** Correctness bug.
- **Location:** `run.rs:443-446, 463-467` captured `screenshot_bytes()` before calling `find_crop_bounds`. Maestro (`Orchestra.kt:1271-1284, 726-738`) runs `findElement` first, then takes the screenshot.
- **Fix:** Swap order to `find_crop_bounds` → `screenshot_bytes`.

### 4. Scroll Strategy: Keep Fling Default, Optimize Around It

- **Severity:** Speed + compatibility.
- **Analysis:** Hold-drag (zero inertia) breaks snapping physics (`PageView`, carousels, `Dismissible` near the 40% threshold). Fling velocity via virtual timestamps matches Maestro's `VelocityTracker` outcome.
- **Concrete wins:**
  - **(a)** Accelerate `swipe` and `scroll` with virtual timestamps (`~240–340 ms` saved per command; ~1.7 s on `f2_swipe_and_scroll.yaml`).
  - **(b)** Shorten `wallDuration` from 64 ms to 16–32 ms (~160–200 ms saved on `scrollUntilVisible`).
  - **(c)** Keep hold-drag strictly opt-in (`--scroll-mode=drag`), flagged as non-differential.
  - **(d)** On iOS, skip waiting for sub-pixel coasting between intermediate swipes; full scroll settle before returning.

### 5. Large Infinite Animations Hit 3 s Settle Timeout Every Step

- **Severity:** Speed.
- **Location:** `animation_scanner.dart:128-145`, `idle_detector.dart:249-253`. Any repeating animation covering ≥ 2% of screen (pulsing CTA, shimmer skeleton, animated gradient) caused +6 s per `tapOn` (pre-tap + post-tap settle).
- **Fix:** Track first-seen timestamp per animating `Element`; classify animations running > 1.5 s with an unchanged semantics tree as `infinite: true` (non-blocking) until they stop.

### 6. iOS Spike Fixes Missing from Main Branch

- **6a. Coasting scroll omitted from settle:** `BallisticScrollActivity.shouldIgnorePointer = true` swallows taps during sub-pixel deceleration. Port `ScrollableState.position.isScrollingNotifier` into `animation_scanner.dart` (with ≥ 2% area threshold and #5 endless guard).
- **6b. Absent soft keyboard:** `keyboard_waiter.dart` waited 700 ms on every field tap under hardware keyboards (`hw.keyboard=yes` on Android AVD or iOS simulator). Port adaptive `absentStartTimeout = 120 ms` (latched after 2 consecutive editable misses, cleared on any late `viewInsets.bottom` change, plus `configure.softKeyboard: false` hint) and reduce `stableWindow` from 64 to 48 ms.

### 7. `launchApp` Default Permissions & `killApp` Semantics

- **Severity:** Maestro divergence, changes pass/fail.
- **Location:** `run.rs:277-302`. Maestro defaults to `permissions: {all: allow}` (`Orchestra.kt:1346`) and implements `killApp` with `am kill` (`AndroidDriver.kt:247-252`), not `am force-stop`.
- **Fix:** Grant runtime permissions from `dumpsys package` via `pm grant` in `launchApp`; switch `killApp` to `am kill`.

### 8. `launchApp`: 5 s Caps, Missing PID Verification, Multiple ADB Spawns

- **Severity:** Speed + diagnostics.
- **Location:** `driver.rs:76` (5 s blocking `hello` read on a stale/half-open socket), `run.rs:291`, missing PID verification.
- **Fix:** Record phase timings (`stopClear`, `permissions`, `amStart`, `accept`, `configure`, `waitIdle`), reject old-PID connections, handle pending `hello` sockets concurrently with a 300 ms read cap, and merge `pm clear`/`resolve-activity`/`am start` into a single `adb shell`.

### 9. Rust Read Timeout Shorter Than Worst-Case Handler

- **Severity:** Correctness (false timeouts).
- **Location:** `driver.rs:101-105`, `rpc_session.dart:22, 34-38`. Worst-case `longPressOn` with `retryTapIfNoChange` + `waitUntilVisible` can reach ~45 s vs a 32 s read timeout, and `_queue` was shared across reconnects.
- **Fix:** Emit `{"event":"progress","id":…}` heartbeats every 2 s from `RpcSession` so the CLI uses a tight 10 s per-read timeout without false drops; scope queues per connection and cancel waiting loops on disconnect.

### 10. Differential Flows and Unit Tests Gaps

- Commit a real Maestro-captured reference PNG (`ref_visual_badge.png`), plus pixel-mismatch and size-mismatch negative checks (`optional: true` + `_diff.png`).
- Copy Maestro's 3 reference PNGs into `compat/fixtures/screenshot/` so the unit test never silently skips.
- Add negative `traits: long-text`, `css: ".x"`, real `Tab` focus verification (`id: field_two`), regex assertions for `inputRandom*`, `PageView` snapping test, `back` with keyboard open, real `waitUntilVisible` overlay, endless large animation flow, negative failing flows, and a widget test verifying `VelocityTracker` velocity under virtual timestamps.
- Save differential logs under `bench/results/f2-differential/`.

### 11. Screenshot Paths: CWD vs Artifacts Dir & `.png` Suffix

- **Severity:** Maestro divergence.
- **Fix:** Introduce `--test-output-dir` (default `./.tapwing/<run>/`), always append `.png` in `takeScreenshot`, and search flow directory + `--test-output-dir` (never CWD) in `assertScreenshot`.

### 12. Unfocused `inputText` / `eraseText` Fails

- **Severity:** Maestro divergence.
- **Fix:** Default to succeeding with a warning (`ime.noClient: true`) matching Maestro; expose opt-in `--strict-input` for `NO_FOCUS` errors.

### 13. ADB `pressKey`: Spawn + Fixed 300 ms Sleep

- **Severity:** Speed.
- **Fix:** Prefer `cmd input keyevent` and replace fixed 300 ms sleep with `waitIdle(300)` when the driver is connected (except `Power`/`Lock`).

### 14. Screenshot: On-Device PNG Encode vs Raw `screencap`

- **Severity:** Speed (candidate to benchmark).
- *(Measured in F2.1: raw 18 MB transfer over `adb exec-out` took 1,665 ms vs 221–350 ms for `screencap -p`, so rejected based on data.)*

### 15. `scrollUntilVisible` Loop Parity

- **Severity:** Minor Maestro divergence.
- **Fix:** Wait up to 500 ms before the first swipe (free when already present) and remove the extra post-loop check.

### 16. `assertDarkMode` Source of Truth

- **Severity:** Minor Maestro divergence.
- **Fix:** Query `cmd uimode night` on the host (matching `AndroidDriver.kt:821-829`), working even when no app is running.

### 17. Hot-Path Snapshots

- **Severity:** Speed (small).
- **Fix:** Use allocation-free `Snapshot.computeHash()` for post-tap change detection and hash-gating `waitFor`/`waitGone`, including `clickable`, `isTextField`, and `scopesRoute` in the hash.

### 18. Sequential CLI Setup & Teardown

- **Severity:** Speed (small).
- **Fix:** Run `reverse` and `keep_awake` setup/teardown concurrently.

### 19. `KNode.error` Never Populated on Flutter

- **Severity:** Cleanup. Document in `packages/tapwing_driver/README.md` that it is always `null` for pure Flutter UI and retained for future native-lane platform-view parity.

### 20. Virtual Pointer Clock Monotonic Drift

- **Severity:** Cleanup. Switch from unbounded additive offset to a high-water mark (`max(raw, last + 1 µs)`) and document in `PLAN.md` §2.2 and driver README.

### 21. Minor Divergences

- `optional` should only downgrade Maestro command errors (never `INTERNAL` or broken sockets); `cropOn.optional` must not make the whole screenshot command optional; `swipe` gives `direction` precedence over `start/end`.

### 22. Risk: `retryTapIfNoChange` vs `kDoubleTapTimeout`

- **Severity:** Plausible risk. Waiting less than `kDoubleTapTimeout` (300 ms) before retrying an unchanged single tap can trigger `onDoubleTap` on widgets with both `onTap` and `onDoubleTap`. Wait past `kDoubleTapTimeout` before issuing the retry.

---

## 2. Verified F2 Claims (Confirmed Against Source)

| Claim | Tapwing | Maestro / Flutter |
|---|---|---|
| `text`/`id` full-match + newline fallback across `text`/`hintText`/`accessibilityText`/`error` | `selector.dart:344-366` | `Filters.kt:58-132` |
| `traits`: `text`, `square` < 3%, `long-text` > 200 | `selector.dart:381-397` | `Filters.kt:220-239`, `TraitFilters.kt` |
| `css` never matches on Android | `selector.dart:410-415` | `Filters.kt:299-316` |
| `deepestMatchingElement`, intersection ordering, stable `clickableFirst` | `selector.dart:322-339, 422-440, 567-570` | `Orchestra.kt:1754-1780`, `Filters.kt:38-43, 254-297` |
| `childOf` subtree of first matching parent; nested `childOf` ignored in relative anchors | `selector.dart:219-240` | `Orchestra.kt:1500-1543, 1613-1622` |
| ≥ 10% visibility; overflowing container = 100% | `selector.dart:244-283` | `ViewHierarchy.kt:102-128`, `UiElement.kt:31-47` |
| Element-relative point (% and px, bounds) | `driver_context.dart:138-163` | `ElementCoordinateUtil.kt` |
| `retryTapIfNoChange` = 2 attempts, tree comparison after settle | `command_handlers.dart:465-527` | `Maestro.kt:403-441` |
| `waitUntilVisible` for center tap when unchanged & occluded (10 s) | `command_handlers.dart:131-153, 531-546` | `Maestro.kt:262-281, 509-522`, `ViewHierarchy.kt:40-95` |
| Long press 3,000 ms | `rpc_messages.dart:32` | `AndroidDriver.kt:281-285` |
| Full-screen and element swipe ratios | `run.rs:548-555`, `command_handlers.dart:550-562` | `AndroidDriver.kt:395-475` |
| `scroll` = swipe UP 400 ms | `run.rs:381-383` | `AndroidDriver.kt:371-375` |
| `speed` → `(1000·(100−s)/100)+1`, negative timeout → 20,000 | `run.rs:559-580` | `Commands.kt:158-169` |
| `centerElement`: visibility > 0.1, max 4 retries, 1/5 margin | `command_handlers.dart:352-378`, `selector.dart:290-320` | `Orchestra.kt:876-886`, `UiElement.kt:49-68` |
| `copyTextFrom` priority `text → hintText → accessibilityText → error → accessibilityText` | `command_handlers.dart:565-577` | `Orchestra.kt:1854-1867` |
| `setClipboard` sets variable; `pasteText` with empty clipboard is no-op | `run.rs:371-379` | `Orchestra.kt:1846-1872` |
| Android keycodes | `run.rs:221-254` | `AndroidDriver.kt:289-320` |
| `matchPercentage` (Euclidean RGB tolerance 0.1, ARGB equality, tolerance 0) | `screenshot.rs:99-126` | `ScreenshotMatch.kt:78-112` |
| Crop bounds (Android scale = 1) | `screenshot.rs:69-85` | `Maestro.kt:652-674` |
| Invalid numeric fields fail even when `optional: true` | `run.rs:117` | `Orchestra.kt:377-381` |

---

## 3. Black-Box Contract

- **Compliant:** all input enters via `onPointerDataPacket`, `onKeyData` (`synthesized: true`), and `flutter/textinput`/`flutter/navigation` via `channelBuffers`. No direct callbacks, `SemanticsAction`s, or `jumpTo`. Screenshots use external `screencap`.
- **Documented explicitly in F2.1:** virtual drag timestamps (#20), `TextInput.hide` sent to engine (#1), and read-only inspection of `ScrollableState.position.isScrollingNotifier` and `ImplicitlyAnimatedWidgetState.animation` for settle detection.

## 4. Rust ↔ Dart Protocol Consistency

Field names, types, defaults, and result payloads align cleanly. Timeouts (#9) and `notVisibleMs` (#2) were resolved in F2.1.

## 5. Emulator Benchmark Time Budget (Excl. `launchApp`, 5,392 ms)

| Component | ms | Reducible without changing semantics? |
|---|---:|---|
| `scrollUntilVisible` (5 × 64 ms drag + ~312 ms fling + frames) | ~2,000 | ~160–200 ms (#4b) |
| Route & dialog transitions (`tapOn "Product 40.*"`, `back`, `Add product`, `Add`, `login_button`) | ~1,730 | No (real app animation) |
| Soft keyboard (`Email`, `password_input`, `Name`, `hideKeyboard`) | ~950 | ~64 ms (#6b `stableWindow`) |
| Simulated login delay (`assertVisible "Products"`) | ~400 | No |
| Other (assertions, `inputText`, ripple taps) | ~300 | Tens of ms (#17) |
| CLI setup + teardown (wall − flow) | ~400 | ~100–150 ms (#18) |

The lower bound under exact Maestro semantics on this flow and emulator is ~5.0–5.2 s excluding `launchApp` (achieved in F2.1 at 5.18 s). Larger savings from #4a, #5, #6b, and #13 appear on flows with `swipe`, endless animations, hardware keyboards, and system keys.

---

## 6. Recommended Fix Order (Completed in F2.1)

1. **#1, #2, #3:** parity fixes (`run.rs`, `command_handlers.dart`).
2. **#10:** strengthen differential flows (`PageView`, `back` with keyboard, negative cases, committed reference screenshot, mandatory fixtures).
3. **#4a + #4b:** accelerated `swipe`/`scroll` and shorter drag duration without changing semantics.
4. **#6a + #5:** coasting scroll and endless animations in settle detector.
5. **#7, #8, #9:** `launchApp` (permissions, PID, phase timings), progress heartbeats, and timeouts.
6. **#6b, #11–#16**, then remaining cleanup.

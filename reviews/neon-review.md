# Tapwing Review (F0 Spike) — Speed and Correctness

Date: 2026-11-20 · Reviewed state: `crates/tapwing` + `packages/tapwing_driver` (F0 v2) against Maestro **2.12.1** and Flutter **3.47.5**.

Findings are ranked by real-world impact (combining measured speed on `login_and_browse.yaml` and correctness/compatibility severity). Each item notes whether it was **applied** immediately or left as a **proposal** for subsequent phases (all major items were later integrated during F1/F2/F2.1).

---

## Summary of Findings by Impact

| # | Area | File:line | Impact | Status |
|---:|---|---|---|---|
| 1 | Idle / keyboard | `idle.dart:142, 185-212` | **~250–400 ms** per flow + fixes premature and repeated keyboard waits | **Applied** |
| 2 | `launchApp` + ADB | `device.rs:33-80`, `main.rs:90-92, 212-230` | **~250–500 ms** on `launchApp` (`1.7 s → ~1.2 s`) | Partially applied + **proposal** |
| 3 | `scrollUntilVisible` | `server.dart:206-234`, `input.dart:70-93` | **~300–500 ms** per flow (from 1,179 ms) + compatibility | **Proposal** |
| 4 | Correctness: mid-transition tap | `server.dart:173-197` | Major source of flakiness after async navigation | **Proposal** (adopted in F0 v3) |
| 5 | Correctness: poisoned RPC queue | `server.dart:75-97` | Permanent driver wedge after host disconnect | **Proposal** (fixed in F1/F2) |
| 6 | Idle: off-by-one + leaks + animation scan | `idle.dart:54-60, 73-178`, `server.dart:240-269` | **~120–200 ms** per flow + eliminates callback leaks and O(N) churn | **Applied** (`idle.dart`) + **proposal** (`server.dart`) |
| 7 | Idle: missing & endless animations + timers | `idle.dart:90-98` | Prevents premature settles and 3 s stalls | Partially applied + **proposal** |
| 8 | Snapshot + selector | `snapshot.dart:97-177`, `selector.dart:97-266` | 2× fewer `SemanticsData`, zero-alloc hash, O(N) vs O(N²), 4 bug fixes | **Applied** |
| 9 | Rust↔driver latency & resilience | `driver.rs:58-131`, `main.rs:90-112`, `server.dart:52` | Eliminates 150 ms startup race + ANR hang guard | **Applied** (Rust) + **proposal** (`server.dart`) |
| 10 | HTTP tracking leaks | `idle.dart:174, 234-250, 344, 439` | Eliminates 10 s WebSocket block and leaked `Timer`s | **Applied** |
| 11 | Parser + Maestro 2.12.1 semantics | `flow.rs:54-263`, `device.rs:62-79`, `main.rs:84-87, 231, 259` | Fixes `index: "-1"`, `${APP_ID}`, string booleans, `am start` stderr | **Applied** + **proposals** |
| 12 | `inputText` platform channel spam | `input.dart:193-261` | 20× fewer JNI/IME sync messages on 20-char input | **Proposal** |

---

## 1. Keyboard Detection: Premature Settle After `tapOn` and Repeated 64 ms Waits

- **Location:** `packages/tapwing_driver/lib/src/idle.dart:142, 185-212`
- **Issue:** `_waitForKeyboard` ran only once at the start of `waitForIdle` (before `TapGestureRecognizer`/`FocusManager` dispatched `TextInput.show`) and never recorded that a transition had already been settled, causing subsequent steps within 1 s to repeatedly wait 64–80 ms.
- **Fix (applied):** Track settled keyboard transitions in `IdleDetector` and re-check `_hasPendingKeyboardChange()` inside the settle loop.

## 2. `launchApp` Overhead (1.7 s vs 0.8 s in Maestro) and ADB Process Spawns

- **Location:** `crates/tapwing/src/device.rs:33-80`, `crates/tapwing/src/main.rs:90-92, 212-230`
- **Issue:** Cold launch spawned 3 separate `adb` processes (`pm clear`, `resolve-activity`, `am start`), always sent default `configure`, and set up `adb reverse` before `Driver::listen` (triggering a 150 ms reconnect sleep).
- **Fix:** Reordered `listen` before `reverse` and skipped redundant default `configure` (**applied**); proposed merging launch commands into one `adb shell` (implemented in F2.1) and replacing `adb` spawns with a persistent ADB smartsocket in F4.

## 3. `scrollUntilVisible` Cost (1,179 ms) and Missing `speed` / `centerElement`

- **Location:** `server.dart:206-234`, `input.dart:70-93`, `flow.rs:134-139`
- **Issue:** 6 swipes took 196 ms each (64 ms drag + 48 ms hold + ~84 ms settle), covered only 50% of screen height, re-parsed point strings per iteration, and lacked Maestro's `speed`, `centerElement`, `waitToSettleTimeoutMs`, and overflow visibility check.
- **Status:** Full Maestro `scrollUntilVisible` semantics implemented in F2/F2.1.

## 4. Critical Correctness Bug: `tapOn` Tapping Mid-Transition

- **Location:** `server.dart:173-197`
- **Issue:** After async navigation (e.g., 600 ms login), `_waitFor` found the target on frame 1 of the route transition and tapped stale coordinates while `ModalRoute` ignored pointers.
- **Proposal (adopted in F0 v3):** Wait for settle and refresh element bounds between `findElement` and tap (matching `Maestro.kt:233-249`), with 0 ms overhead when already idle.

## 5. Critical Correctness Bug: Poisoned `_queue` on Socket Disconnect

- **Location:** `server.dart:56-98`
- **Issue:** Uncaught `socket.write` errors in `catch` branches rejected the singleton `_queue`, permanently wedging the driver across subsequent connections.
- **Proposal (fixed in F1/F2):** Guard writes with a `closed` flag, attach `catchError`, destroy closed sockets, and isolate queues per connection.

## 6. Idle & Reactive Waits: Off-by-One `stableFrames`, Post-Frame Callback Leaks, Redundant Scans

- **Location:** `idle.dart:54-60, 145-178`, `server.dart:240-269`
- **Issue:** `stable` reset to `0` on first frame (requiring 4 frames instead of 3), `Future.any([nextFrame(), delayed(100ms)])` leaked uncancelable post-frame callbacks, and `Snapshot.capture()` allocated full `KNode` trees just to compute a hash.
- **Fix (applied):** Shared `_frameWaiter`, cancelable `waitForNextFrame(Duration)`, allocation-free `Snapshot.computeHash()`, and fixed `stable` counter. *(Note: the cross-frame animation blocker cache was later removed after checking muted `TickerMode` routes.)*

## 7. Idle Detector: `ImplicitlyAnimatedWidget`, Endless Large Animations, Coasting Scrolls, and App Timers

- **Location:** `idle.dart:87-118`
- **Issue:** Scanner missed `ImplicitlyAnimatedWidget` (`AnimatedContainer`, etc.) and `Offstage(offstage: true)` (**both applied**). Flagged endless large animations (3 s timeout every step — resolved in F2.1), ballistic scroll coasting (`isScrollingNotifier` — resolved in F2.1), and short app `Future.delayed` timers.

## 8. Snapshot & Selector Performance and Compatibility Bugs

- **Location:** `snapshot.dart:113-171`, `selector.dart:97-266`
- **Fixes (applied + unit tested):**
  - Eliminated duplicate `SemanticsNode.getSemanticsData()` calls (2× fewer allocations).
  - Allocation-free `Snapshot.computeHash()` including flags, `hint`, `tooltip`, and `identifier`.
  - Cached compiled `RegExp` on `Selector` + fast-path `matchesText` without `replaceAll('\n', ' ')`.
  - O(N) ancestor-marking `deepestMatchingElement` (replacing O(N² × depth)).
  - Fixed unbalanced-parenthesis regex fallback (`toRegexSafe`), overflowing-element 100% visibility (`UiElement.kt:36-39`), offscreen child filtering in `containsDescendants`, and `resolveAll` respecting `childOf`. *(Note: empty `resource-id` matching was subsequently restored to match Maestro's `resource-id=""`.)*

## 9. Rust ↔ Driver Latency & Resilience

- **Location:** `driver.rs:58-131`, `main.rs:90-112`, `server.dart:52`
- **Fixes (applied in Rust):** Dynamic read timeout (prevents infinite hangs on ANR), deadline-checked `accept` ignoring malformed/stale connections, and `listen` before `reverse`. Proposed exponential reconnect backoff (25 → 150 ms) in `TapwingServer._loop()`.

## 10. HTTP Tracking Leaks & Polling (`idle.dart`)

- **Fixes (applied):** Cancel 10 s safety timers on response completion, release `_httpInFlight` on `detachSocket()` (eliminating 10 s WebSocket blocks) and aborted requests, and wake idle waiters immediately via `Completer` instead of 4 ms polling.

## 11. YAML Parser & Maestro 2.12.1 Parity Fixes

- **Fixes (applied in F0 prototype, superseded by full F1 parser):** Signed `index: "-1"`, `${APP_ID}` interpolation, string boolean coercion, rejecting malformed `-e FOO` without `=`, and reading combined stdout/stderr from `am start`. Also flagged `assertNotVisible` 17 s timeout, `killApp` = `am kill`, element-relative `tapOn`, and Maestro swipe ratios (all resolved in F2/F2.1).

## 12. `inputText` / `eraseText` Native IME Sync Spam

- **Location:** `input.dart:193-261`
- **Proposal:** Defer `TextInput.setEditingState` sync to once at the end of `type()`/`erase()` rather than sending a platform-channel message after every character, cutting JNI/IME chatter 20× on 20-character inputs while still running `TextInputFormatter`s and `onChanged` per character.

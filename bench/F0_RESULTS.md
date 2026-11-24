# F0–F2.1: Spike & Benchmark Results

Date: 2026-11-20 · Android Emulator Pixel 6 Pro (Android 15, 1440×3120 @ 560 dpi) · Flutter 3.47.5, arm64 profile build · Maestro 2.12.1 · Tapwing (Rust CLI + `tapwing_driver`)

Reproduction: `bench/run_bench.sh 3 emulator-5554` (3 runs per tool, medians). Raw data: `bench/results/emulator-5554/`. The same comparison on a physical Pixel 8a is in [Pixel 8a (Real Device)](#pixel-8a-real-device).

## Summary

Tapwing F0 was measured in two versions: **v1** = initial spike, **v2** = after fixing settle detection (see "Settle Fix (v2)" below). The Maestro baseline is unchanged (3 runs, same APK, flow, and emulator).

| Metric | Maestro | Tapwing v1 | Tapwing v2 | v2 Speedup |
|---|---:|---:|---:|---:|
| End-to-end incl. CLI startup (wall clock) | 52.5 s | 6.6 s | 6.3 s | **8.3×** |
| Sum of commands | 45.9 s | 6.5 s | 6.2 s | **7.4×** |
| Sum of commands excluding `launchApp` | 45.1 s | 4.8 s | 4.4 s | **10.2×** |
| Median per-command speedup (excl. `launchApp`) | | 14× | | **18×** |
| Actions returned mid-animation (`--check-settle`) | | 2 / run | **0** (3 runs) | |
| Element tree dump (`hierarchy`) | 7.0 s | 0.16 s | | 44× |
| CLI startup + device attach | ~6.6 s (JVM + driver) | 22–41 ms | | |

**F0 acceptance criterion** (≥ 10× per-command): met. Median speedup is 18× and even the full flow excluding `launchApp` exceeds 10× after the settle fix.

**After Pixel fixes (v3)** on the emulator (Tapwing 3 runs, same Maestro baseline):
- Wall clock 6.7 s (**7.8×**), as CLI startup now checks screen wake and lock state (~75 ms)
- Excluding `launchApp` 4.5 s (**10.0×**)
- 0 settle violations

On a physical **Pixel 8a**, the gap widens: **11.4×** end-to-end and **11.9×** excluding `launchApp`. See [Pixel 8a (Real Device)](#pixel-8a-real-device).

**Compatibility:** the exact same flow passes in both tools (3/3 Tapwing, 3/3 Maestro). A control run of Maestro on the *instrumented* APK also passed, confirming the driver does not interfere with Maestro.

## Settle Fix (v2)

**Problem (v1):** while animations kept ticking, the driver reported "settled" after 8 frames without Semantics tree changes. Material page transitions (`PredictiveBackPageTransitionsBuilder` → `DualTransitionBuilder` / `SnapshotWidget`) and dialog entrances animate paint only, leaving the semantics tree unchanged. As a result, `back` and opening a dialog returned mid-animation.

**Solution (v2):** "settled" now requires both 3 consecutive unchanged frames **and no significant running animation** covering ≥ 2% of the screen. The driver inspects only public transition widgets:
- `AnimatedWidget` (Slide/Scale/Size/… transitions, `AnimatedBuilder`, `ListenableBuilder`)
- `DualTransitionBuilder`
- `FadeTransition` / `SliverFadeTransition`

For each one, it checks `Animation.isAnimating` and its render box area, skipping subtrees with disabled tickers (`TickerMode(enabled: false)`, i.e., covered routes). This is purely read-only observation, preserving the black-box contract.

- Small infinite animations (button spinners ~0.1% of screen, blinking cursor) and local repaints (ink ripples) do not block settle.
- Route transitions, dialogs, snackbars, and hero flights do block settle.

**Verification:** `tapwing test --check-settle` queries the driver (`busy`) after every action to confirm no significant animation is still running. Same build and flow:

| Policy | Violations | Details | Flow time |
|---|---:|---|---:|
| `--settle-policy legacy` (v1) | 2 | `back`: `DualTransitionBuilder`/`FadeTransition`/`SlideTransition` 100% of screen; `tapOn "Add product"`: `AnimatedModalBarrier`/`FadeTransition` 100% | 7.4 s |
| `--settle-policy v2` | **0** | | 6.1 s |

Bonus effect: the shorter stability window (3 instead of 8 frames) sped up taps that only trigger an ink ripple — e.g., `tapOn "Email"` 165 → 74 ms, `tapOn id: favorite_switch` 146 → 58 ms, `tapOn "Name"` 330 → 63 ms. Conversely, `back` became longer (263 → 493 ms) because it now waits for the route transition to finish, dropping the following `assertVisible "Products"` from 218 to 2 ms.

Also fixed a bug in keyboard waiting: the 700 ms start-timeout was measured from the start of `waitForIdle` instead of from the keyboard show/hide request.

## Pixel 8a (Real Device)

Pixel 8a (Android 16, 1080×2400 @ 420 dpi, Gboard), USB · same flow · `OUT=bench/results/pixel-8a bench/run_bench.sh 3 <serial>` · data: `bench/results/pixel-8a/`. Maestro 3 runs (one retried, see finding 1), Tapwing 3 runs. **v2** is the initial measurement, **v3** is after the Pixel fixes and review below.

| Metric | Maestro | Tapwing v2 | Tapwing v3 | v3 Speedup |
|---|---:|---:|---:|---:|
| End-to-end incl. CLI startup (wall clock) | 75.2 s | 6.2 s | 6.6 s | **11.4×** |
| Sum of commands | 66.3 s | 5.9 s | 6.0 s | **11.0×** |
| Sum of commands excluding `launchApp` | 63.2 s | 5.2 s | 5.3 s | **11.9×** |
| Median per-command speedup (excl. `launchApp`) | | 24× | 12× | |
| `launchApp (clearState)` | 3.1 s | 0.7 s | 0.7 s | 4.5× |
| Actions returned mid-animation (`--check-settle`) | | 0 | 0 | |

v3 added startup checks verifying the screen is awake and unlocked (~320 ms on Pixel, ~75 ms on emulator), explaining the slight wall-clock increase.

The per-command median dropped only because time shifted to the right command, not because execution slowed down. Waiting for the soft keyboard is now properly accounted for in `tapOn` rather than spilling into `inputText`:

| Command | v2 | v3 |
|---|---:|---:|
| `tapOn "Email"` | 63 ms | 567 ms |
| `inputText "jan@example.com"` | 483 ms | 31 ms |

Total flow time stayed identical.

A control run of Maestro on the driver-instrumented APK passed.

**Hierarchy parity** at 420 dpi (different px/dp ratio from emulator): Products 14/14 and Detail 5/5 nodes match, with pixel-exact bounds. The only difference is 3 system status-bar nodes (notifications, battery, ringer) outside the app.

**Why the lead is larger on a real device:**
- Maestro is 43% slower on the Pixel than on the emulator (75 s vs 52.5 s). UiAutomator hierarchy dumps are slower on device: a single `assertVisible` takes ~200 ms vs ~70 ms on emulator, and `tapOn` takes 2.6–4.4 s.
- Tapwing runs at roughly the same speed on both because it reads the Semantics tree in-process.
- Part of Tapwing's time is the real soft keyboard (Gboard): show ~500 ms, `hideKeyboard` ~415 ms (vs 118 ms on emulator).
- `launchApp` on the Pixel is faster in Tapwing (0.7 s vs Maestro's 3.1 s; on emulator Maestro was 0.8 s).

**Real-device findings (all fixed in v3):**

1. **Screen sleep timeout.**
   - Problem: in-process input does not count as OS user activity on Android, so the display timeout keeps ticking. After ~10 minutes without a physical touch, the Pixel slept (`mLastSleepReason=timeout`), halting frame rendering and causing taps and settle loops to time out.
   - Impact: broke Maestro's first run (black screen, retried) and 4 Tapwing runs during 16:10:44–16:14:20.
   - Fix: CLI sets `stay_on_while_plugged_in=15` at startup and restores the original value on exit (avoiding `svc power stayon` due to its ~1 s JVM startup). Sleeping screens are woken; non-secure lockscreens are dismissed via `wm dismiss-keyguard`; secure lockscreens fail fast with exit code 2 instead of 17 s timeouts.
   - Verified: sleeping Pixel woke and stayed awake; locked Pixel failed immediately with "device is locked"; setting restored to `0` on exit.
   - Limitation: unplugged devices over Wi-Fi ADB are not covered by `stay_on_while_plugged_in`.
2. **Cross-device port collision (root cause of earlier "flake").**
   - Problem: all runs shared host port 7781. An app left running on another device kept reconnecting through its old `adb reverse`, and the CLI accepted whichever socket arrived first.
   - Repro: an emulator run ended up driving the app on the Pixel (3/3 failures).
   - Fix: each session binds an ephemeral host port (`--port 0` default) and cleans up its `adb reverse` on exit.
   - Verified: emulator runs pass 3/3 even while the app keeps running on the Pixel.
3. **Keyboard & settle timing.**
   - Problem: `tapOn` on a text field returned before Gboard began sliding up, because `TextInput.show` fires in a microtask after focus changes.
   - Fix: yield a zero-duration timer after tapping and re-check pending keyboard requests inside the settle loop.
4. **Flows without `launchApp`** (attaching to an already running app) failed with "app is not connected".
   - Fix: CLI attaches lazily on the first driver command (up to 5 s) and always sends `configure`.
   - Verified on Pixel.

The driver also logs `flutter/textinput` traffic and taps into `tapOn` / `inputText` responses (`ime.log`) for post-mortem debugging.

## Review (Gemini 4 Neon) and Adopted Changes

Review notes: [reviews/neon-review.md](../reviews/neon-review.md). Adopted improvements:
- Cheaper hash and snapshot (no second `getSemanticsData`, allocation-free hashing)
- Regex caching in selectors and O(N) `deepestMatchingElement`
- Shared post-frame waiter (no callback or timer leaks)
- HTTP tracking without 10 s hangs on WebSockets
- Typed errors and timeouts in the Rust client + unit tests

Two items were reverted/adjusted after checking Maestro's source:
- **`id` on empty `resource-id`:** Maestro always emits `resource-id=""`, so `id: ".*"` matches it. Restored and test updated.
- **Cross-frame animation blocker cache:** removed. An animation inside a newly covered route has its ticker muted and appears "animating" forever, which would stall settle until timeout.

Also adopted re-aiming `tapOn` after settle (matching `Maestro.tap`), costing 0 ms when already idle.

## After F1 & Driver Refactor

Runtime runs over the F1 parser IR and the Dart driver was refactored into typed modules (strict lint, 27 tests). Tapwing runs repeated (`TAPWING_ONLY=1`), Maestro baseline unchanged:

| Device | End-to-end | Excl. `launchApp` | Settle violations |
|---|---:|---:|---:|
| Pixel 8a | 6.5 s (**11.6×**) | 5.3 s (**11.9×**) | 0 in 3 runs |
| Emulator | 6.6 s (**8.0×**) | 4.5 s (**10.1×**) | 0 in 3 runs |

Numbers match v3, confirming zero regression from the refactor.

## Per-Command Breakdown (Medians, ms, Tapwing v1)

| # | Command | Maestro | Tapwing | Speedup |
|---|---|---:|---:|---:|
| 1 | `launchApp (clearState)` | 792 | 1679 | 0.5× |
| 2 | `assertVisible "Welcome back"` | 1576 | 7 | 218× |
| 3 | `tapOn "Log in"` | 2571 | 169 | 15× |
| 4 | `assertVisible "Email is required"` | 69 | 2 | 37× |
| 5 | `tapOn "Email"` | 2871 | 165 | 17× |
| 6 | `inputText "jan@example.com"` | 2282 | 285 | 8× |
| 7 | `tapOn id: password_input` | 2862 | 201 | 14× |
| 8 | `inputText "secret123"` | 1715 | 85 | 20× |
| 9 | `hideKeyboard` | 901 | 118 | 8× |
| 10 | `tapOn id: login_button` | 2114 | 232 | 9× |
| 11 | `assertVisible "Products"` | 1047 | 406 | 3× |
| 12 | `assertVisible "Product 1.*"` | 68 | 3 | 25× |
| 13 | `scrollUntilVisible "Product 40.*"` | 11673 | 1092 | 11× |
| 14 | `tapOn "Product 40.*"` | 2508 | 498 | 5× |
| 15 | `assertVisible "Details of Product 40"` | 68 | 4 | 18× |
| 16 | `tapOn id: favorite_switch` | 2089 | 146 | 14× |
| 17 | `assertVisible "Favorite"` (checked) | 70 | 3 | 21× |
| 18 | `back` | 1146 | 263 | 4× |
| 19 | `assertVisible "Products"` | 67 | 218 | 0.3× |
| 20 | `tapOn "Add product"` | 2092 | 165 | 13× |
| 21 | `assertVisible "New product"` | 69 | 3 | 28× |
| 22 | `tapOn "Name"` | 2855 | 330 | 9× |
| 23 | `inputText "Tapwing"` | 1469 | 153 | 10× |
| 24 | `tapOn "Add"` | 2299 | 283 | 8× |
| 25 | `assertVisible "Added Tapwing"` | 69 | 3 | 24× |
| 26 | `assertNotVisible "New product"` | 570 | 2 | 355× |

Driver `tapOn` breakdown (median): **element lookup 0.1–0.2 ms, tap injection 0.3–4 ms, remainder is settle time** (140–495 ms: ink ripples, route transitions, keyboard). The driver itself is effectively free; remaining time is real app animation.

## Compatibility: Differential Hierarchy Comparison

`bench/tools/diff_hierarchy.py` compares `maestro hierarchy` against `tapwing hierarchy` on identical screens (`text`, `accessibilityText`, `hintText`, `resource-id`, `bounds`, `clickable`, `checked`, `enabled`):

| Screen | Matching | Differences |
|---|---:|---:|
| Products (list) | 14 / 14 | 0 (+1 system status bar icon outside app) |
| Detail (switch) | 5 / 5 | 0 (+ same system icon) |

Bounds match **to the exact pixel**, and string composition matches Flutter 3.47.5 Android `AccessibilityBridge` / `BaseRoleConfigurator`:
- Text fields: `text` = value, `hintText` = `"label, hint"`
- Everything else: `accessibilityText` = `"value, label, hint"`

Two cases where the initial draft flow failed in Tapwing also failed in Maestro (verified against Maestro's hierarchy):
1. `ListTile` merges title and subtitle into `"Product 1\n€ 3.00"`, so full-match `"Product 1"` does not match; `"Product 1.*"` is required.
2. `Semantics(identifier:)` wrapping `SwitchListTile` creates a wrapper node with `checked=false`; `checked=true` lives on the child `"Favorite"`.

## F0 Follow-Up Findings

1. **`launchApp` is slower (1.7 s vs 0.8 s).** Maestro returns immediately after launching the process and pays first-screen wait in the next command (`assertVisible "Welcome back"` 1.6 s). Launch + first assert: Maestro 2.4 s, Tapwing 1.7 s. Persistent ADB (F4) will further eliminate process spawn overhead.
2. ~~"Stable tree = settled" heuristic fails on route transitions.~~ **Fixed in v2.**
3. ~~Post-tap settle costs 140–330 ms due to ink ripples.~~ **Fixed in v2** (3-frame stability window); ripple taps drop to 58–90 ms.
4. **Keyboard:** native IME animations only surface in Flutter via `viewInsets`. The driver waits explicitly (up to 700 ms from request to start moving, then stable window). Limit bug fixed in v2; adaptive hardware-keyboard handling added in F2.1.
5. **Remaining:** app `Future.delayed` / timers (e.g., 600 ms simulated login) are invisible to the idle detector. Correctness is maintained by reactive waiting in the next command.

## What F0 Validated

- Rust CLI → `adb reverse` → app dials out. Handshake works cleanly in profile builds.
- Engine → framework boundary input works: touches (`PointerDataPacket`), text (`flutter/textinput` + native IME sync), back (`flutter/navigation` `popRoute`), keyboard hide.
- Maestro selector semantics: `IGNORE_CASE|DOT_MATCHES_ALL|MULTILINE` full-match regex, `deepestMatchingElement`, `clickableFirst`, y/x `index` sorting, `< 10%` visibility filter, relative selectors matching `Filters.relativeTo`.
- Driver is invisible to production `main.dart`: dev dependency + `lib/main_tapwing.dart` only.

## F2.1

After addressing all findings from [reviews/f2-review.md](../reviews/f2-review.md) and applying speed optimizations while preserving exact Maestro 2.12.1 semantics (including Maestro-equivalent fling velocity in `scrollUntilVisible`, default permission grants in `launchApp`, PID verification, and coasting-scroll settle detection). Measured on `emulator-5554` (`TAPWING_ONLY=1 bench/run_bench.sh 3 emulator-5554`, 3/3 passes, 0 settle violations):

### Benchmark `login_and_browse.yaml` (Medians of 3 Runs)

| Metric | Maestro 2.12.1 | F2 (before) | F2.1 (after) | F2→F2.1 Delta | Speedup vs Maestro |
|---|---:|---:|---:|---:|---:|
| End-to-end incl. CLI startup (wall clock) | 52.5 s | 7.6 s | **7.2 s** | **−400 ms** | **7.3×** |
| Sum of commands | 45.91 s | 7.17 s | **6.90 s** | **−264 ms** | **6.7×** |
| Sum of commands excluding `launchApp` | 45.12 s | 5.39 s | **5.18 s** | **−216 ms** | **8.7×** |
| `launchApp (clearState)` | 792 ms | 1,776 ms | **1,728 ms** | **−48 ms** (incl. `permissions: all: allow` + PID + phase timings) | 0.5× |
| `scrollUntilVisible "Product 40.*"` (5 swipes) | 11,673 ms | 2,009 ms | **1,878 ms** | **−131 ms** (incl. final scroll settle) | **6.2×** |
| `hideKeyboard` | 901 ms | 115 ms | **97 ms** | **−18 ms** (`stableWindow` 64 → 48 ms) | **9.3×** |

Excluding `launchApp` (5.18 s) sits right at the lower bound of the review §5 theoretical budget (~5.0–5.2 s) under strict Maestro semantics.

### Optimizations Outside `login_and_browse.yaml` (Before / After on `emulator-5554`)

| Item (Review Finding) | Before (F2) | After (F2.1) | Notes |
|---|---:|---:|---|
| 6× `swipe`/`scroll` in `f2_swipe_and_scroll.yaml` (#4a/#4b) | 4,228 ms | **2,201 ms** (**−2,027 ms**) | Virtual timestamps (`wallDuration: 16 ms`), identical fling velocity (`PageView` + `Dismissible` pass) |
| Large endless animation (#5, `f2_endless_animation.yaml`) | ~6,000 ms / tap (2× 3 s timeout) | **2,013 ms** first entry (1.5 s classification), then **95 ms / tap** | Keyed by `Element`, 0 `--check-settle` violations |
| `pressKey: Volume Up` (#13) | 537 ms | **244 ms** (**−293 ms**) | `cmd input keyevent` + `waitIdle(300)` instead of fixed 300 ms sleep |
| Raw `screencap` vs `screencap -p` (#14) | 221–350 ms (`-p`, 64 KB) | 1,665 ms (raw, 18 MB) | **Rejected based on measurement:** transferring 18 MB over `adb exec-out` is 4.8× slower than `-p` |
| `launchApp` breakdown (#8, single merged `adb shell` + PID) | 2–3 spawns, 5 s hello cap | `stopClear` ~49 ms, `permissions` ~40–59 ms, `amStart` ~42–77 ms, 300 ms parallel hello | Verified catching `rejectedConnections: 1` on restart |

The full differential suite (7/7 positive + 2/2 negative flows against Maestro 2.12.1 on `plain.apk`) is recorded in [bench/results/f2-differential/summary.md](results/f2-differential/summary.md).

### Independent F2.1 Verification

- **Emulator:**
  - Re-ran benchmark 3/3: 5.21 s excluding `launchApp`, 7.5 s wall clock, 0 settle violations.
  - All 6 F2 flows passed with `--check-settle`; `f2_dark_mode` with night mode enabled and restored afterwards.
  - Both negative flows fail on the expected step.
  - Re-ran Maestro 2.12.1 on `f2_text_clipboard_keys`, `f2_selectors_and_taps`, and `f2_swipe_and_scroll`; all passed.
- **Fixed race condition in `launchApp`:**
  - One negative run failed in 252 ms with "app closed the connection during `configure`", caused by a dying previous app instance sending `hello`.
  - Root cause: `pidof` immediately after `am start` can return empty or include the dying process's PID.
  - Fix: the CLI captures existing PIDs before stopping (`TAPWING_OLDPIDS`), waits for a new PID after launch, and never accepts any old PID. Reported under `launchApp.driver.oldPids`.
  - Stress test of 15 consecutive sessions: 15/15 passed, `launchApp` remained ~1.7 s.
- **iOS Simulator** (main-branch driver): iOS variant of the flow 3/3, ~13.1 s excluding `launchApp`, 0 settle violations. Raw data in `spikes/ios/data/f21-main-driver/`.

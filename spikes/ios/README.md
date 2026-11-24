# iOS Spike (Simulator)

> **Status 2026-11-21:** the full iOS Simulator phase built on this spike is complete. The items under "What Remains" are implemented in `crates/tapwing/src/device/ios.rs` and `packages/tapwing_driver`; results are in [bench/IOS_RESULTS.md](../../bench/IOS_RESULTS.md). The `patched-files/` prototypes are kept for reference only. The tables below describe the state at spike time ("Tapwing today" = before the iOS phase).

Date: 2026-11-20 · Xcode 27.2, iOS 27.2, iPhone 18 Pro simulator (402×874 pt, @3x) · Flutter 3.47.5 · Maestro 2.12.1 · Scope: **simulator only**, physical iOS devices will not be supported.

The spike ran in an isolated copy of the project while F2 was modifying the driver and CLI. Reference files from the spike are archived in `patched-files/`; scripts and measurements are stored here.

## Summary

- **The driver runs on iOS without modifications.** Semantics snapshots, selectors, engine → framework input injection, and outbound sockets (`dart:io`) work identically to Android. The app connects ~1.2–1.5 s after launch; snapshots take 2–4 ms.
- **The full benchmark flow passes** (Tapwing 3/3).
  - Maestro passed only 1 out of 3 runs on the same build (`scrollUntilVisible` failed twice).
  - Maestro's `hideKeyboard` fails completely with Flutter on iOS (3/3 failures), so it is omitted from the iOS benchmark flow. Root cause and verified fixes are documented in [hide-keyboard/REPORT.md](hide-keyboard/REPORT.md).
- **Verification of the F2.1 main-branch driver** (same simulator, 3 runs):
  - Passes 3/3 in ~13.1 s excluding `launchApp` with 0 settle violations (down from 15.5 s in the spike).
  - `scrollUntilVisible` dropped from 9.1 → 6.9 s; `tapOn "Product 40.*"` from 1.18 → 0.52 s; third text field from 808 → 232 ms.
  - The first two text fields still wait ~820 ms for a soft keyboard because adaptive waiting activates after two consecutive misses. Item 3 under "What Remains" resolves this.
- **Speed** (debug build; the simulator does not support Flutter profile builds):

  | | Maestro | Tapwing | After spike tweaks |
  |---|---:|---:|---:|
  | Sum of commands | 45.5 s | 18.3 s (2.5×) | ~17 s |
  | Excluding `launchApp` | 43.5 s | 15.5 s (2.8×) | |
  | End-to-end incl. CLI startup | 57.7 s | 18.5 s (3.1×) | |
  | Median per-command speedup | | 8.6× | |

  Speedup on Android is 11.9×. The narrower iOS lead comes from three fixable factors detailed below: scroll coasting, soft-keyboard waiting, and app reinstall during `launchApp`.

## How Tapwing Connects on iOS

- **No port forwarding needed.** The simulator shares loopback with macOS, so nothing like `adb reverse` is required.
- **Per-run ephemeral port.** `Platform.environment` is **empty** in Flutter on iOS (verified: 0 entries even when `simctl launch` passes `SIMCTL_CHILD_TAPWING_PORT`). Therefore the CLI writes the port to `<app data container>/tmp/tapwing_port` before launch, and the driver reads it from `Directory.systemTemp`. Zero plugins required, and multiple simulators can run concurrently without port collisions.
- **Process verification.** A dying previous app instance can still connect in time. The CLI accepts `hello` only from the PID returned by `simctl launch` (the same PID verification was also adopted for Android in F2.1).
- **`clearState`** = terminate and reinstall the app, matching Maestro (`LocalSimulatorUtils.clearAppState`). For a 139 MB debug bundle this takes ~2.8 s.

## UI Hierarchy Differences on iOS

Maestro on iOS (`IOSDriver.mapViewHierarchy`) composes attributes differently from Android:

| Maestro attribute | iOS source | Tapwing today (Android composition) |
|---|---|---|
| `bounds` | Frame in **points** | Physical px (÷ dpr matches to the exact point) |
| `accessibilityText` | `label` | `label` (+ `value` + `hint`) |
| `text` | `title`, falling back to `value` when empty | Text field `value` |
| `hintText` | `placeholder` | Text field `label` |
| `resource-id` | `identifier` | `identifier` (identical) |
| `checked` | Switch with `value == "1"` | `checked` flag |
| `focused` | (Practically always `false`) | `focused` flag |
| `clickable` | Attribute does not exist | Derived from tap action |

Verified across 5 screens (`data/*-maestro.json` vs `data/*-tapwing.json`). Once translated according to this table, every app node matches:
- **Text field** "Email": `accessibilityText=Email` and `text=jan@example.com` (Tapwing currently emits `hintText=Email`).
- **Switch** "Favorite": `text="1"` and `checked=true` (Tapwing currently sets only `checked`).
- **List items:** identical `"Product 1\n€ 3.00"` composition.

Maestro also includes a root app node (`accessibilityText = "Sample App"`) and status bar elements.

## Maestro iOS Behaviors to Match

- **`back` is a no-op** (`IOSDriver.backPress() {}`). The iOS flow variant therefore uses `tapOn: "Back"`.
- **`hideKeyboard`** performs two center-screen micro-swipes followed by a keyboard visibility check. With Flutter it fails for two independent reasons (full evidence and tested patches in [hide-keyboard/REPORT.md](hide-keyboard/REPORT.md)):
  1. **Hardware keyboard OFF:** the soft keyboard is actually on screen. Flutter on iOS intentionally does not unfocus on tap/drag outside a `TextField` (`editable_text.dart`), so micro-swipes never dismiss it.
  2. **Hardware keyboard ON:** Maestro reports a false positive. Its XCTest runner only checks `keyboards.firstMatch.exists`, and UIKit keeps the soft keyboard in the accessibility tree positioned below the screen (`y = 918 pt` on an `874 pt` screen).
  - **Tested fixes:**
    - In-app: global tap-outside-unfocus — 3/3 pass in both modes.
    - Maestro patch (runner + client, `hide-keyboard/maestro-hideKeyboard.patch`) — 3/3 pass; runner frame-check is worth upstreaming.
    - Simulator settings alone do not help (0/7).
  - **Decision for Tapwing:** do not replicate Maestro's failure. `hideKeyboard` should succeed as on Android (keyboard hidden, focus kept) while logging a compatibility warning, with an optional strict mode emulating Maestro iOS. Define "keyboard visible" as intersecting the screen.
  - Even when Maestro's iOS `hideKeyboard` succeeds, it takes 4–6 s because it waits out two 2-second timeouts.
- **`scrollUntilVisible`** in Maestro on iOS with Flutter is flaky (2/3 failures with "No visible element found").

## Cross-Platform Findings

1. **Settle ignored coasting scrolls (bug — fixed in F2.1).**
   - On iOS (`BouncingScrollPhysics`), lists keep coasting at sub-pixel speed for hundreds of ms after a drag while the semantics hash stays unchanged.
   - Flutter ignores pointer taps during an active scroll (`ScrollActivity.shouldIgnorePointer`), causing `tapOn "Product 40.*"` to be swallowed.
   - Fix: treat `ScrollableState.position.isScrollingNotifier` as a significant animation (`patched-files/driver/animation_scanner.dart`, merged into main in F2.1).
2. **Waiting for an absent soft keyboard (fixed in F2.1).**
   - With hardware keyboard enabled, no soft keyboard appears, causing every field tap to wait 700 ms unnecessarily.
   - Fix: adaptively drop the start timeout to 120 ms once misses are detected (`keyboard_waiter.dart`, merged into main in F2.1). Passing simulator hardware-keyboard state from CLI in `configure` will eliminate the wait on the first two fields as well.
3. **Scroll inertia (note updated after F2 review).**
   - Hold-drag without inertia (48 ms hold before pointer-up) reduced iOS `scrollUntilVisible` from 9.1 s to 6.4 s, and fast 64 ms drags would reach ~1.5 s.
   - However, F2 review proved that removing fling velocity *does* change pass/fail for snapping scrollables (`PageView`, carousels, `Dismissible`), so Maestro-equivalent fling velocity remains the default. Hold-drag is reserved as an opt-in mode only.

## What Remained for the Full iOS Phase (done, see [bench/IOS_RESULTS.md](../../bench/IOS_RESULTS.md))

1. **Simulator control** in the CLI via `simctl`: install, launch with port file & PID verification, terminate, `clearState` via reinstall, screenshot, permissions (`simctl privacy`), `openLink` (`simctl openurl`), `setLocation` (`simctl location`), dark mode (`simctl ui appearance`). Prototype is in `patched-files/cli/device.rs`.
2. **iOS hierarchy composition in the driver** per the table above: points instead of px, Maestro iOS attributes, root app element (name passed by CLI), `focused=false`, no `clickable`. Point coordinates must also apply to `width`/`height`/`point` selectors and swipe coordinates.
3. **iOS command semantics:**
   - `back` is a no-op.
   - `hideKeyboard` succeeds with a compatibility warning; optional strict mode.
   - `pressKey` matching `IOSDriver`.
   - CLI passes simulator hardware-keyboard state (`softKeyboard: false` in `configure`).
4. **iOS differential test suite:** tree comparison like `bench/tools/diff_hierarchy.py` and flows passing in both Tapwing and Maestro on the simulator.
5. ~~Upstream spike fixes into main~~: done in F2.1 (coasting scroll settle, adaptive keyboard waiter, PID verification).

## Files

- `spike_client.py`: minimal NDJSON driver client (file-based port handoff, PID verification, step timings)
- `bench_ios.sh`, `cmp_bench.py`: Maestro vs Tapwing simulator benchmark
- `cmp.py`: UI hierarchy comparison
- `login_and_browse_ios.yaml`: iOS variant of the benchmark flow (iOS bundle ID, `tapOn: "Back"`, no `hideKeyboard`)
- `data/`: captured Maestro and Tapwing hierarchies, baseline summary, and F2.1 main-branch verification logs (`data/f21-main-driver/`)
- `patched-files/`: isolated spike changes (driver: port file, scroll settle, adaptive keyboard; CLI: `IosSim`, `Device`, PID verification, no-op `back`)

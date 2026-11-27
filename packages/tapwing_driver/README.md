# `tapwing_driver`

In-app Flutter driver for **Tapwing**, a Flutter-first black-box runner for Maestro YAML flows.

Linked into the application under test purely as a `dev_dependency` via a separate entrypoint (for example `lib/main_tapwing.dart`), leaving production code untouched:

```dart
import 'package:tapwing_driver/tapwing_driver.dart';
import 'package:my_app/main.dart' as app;

void main() {
  TapwingBinding.ensureInitialized();
  app.main();
}
```

If the application already uses a custom binding, mix `TapwingBindingMixin` in **last** so its `BinaryMessenger` wrapper observes `flutter/textinput`.

The driver runs in profile and debug builds. In a release build it stays off, and the binding behaves like the one it extends, unless the app is built with `--dart-define=TAPWING_ALLOW_RELEASE=true`.

## Installation & Version Sync

Run `tapwing init` in your Flutter project (or Dart pub workspace) to add `tapwing_driver` under `dev_dependencies` and generate `lib/main_tapwing.dart` automatically:

```yaml
dev_dependencies:
  tapwing_driver:
    git:
      url: https://github.com/tapwing-dev/tapwing.git
      path: packages/tapwing_driver
      ref: v0.1.1
```

- **Self-contained git dependency:** `tapwing_driver` keeps `publish_to: none` and depends exclusively on the Flutter SDK (`flutter: sdk: flutter`), with no local path dependencies, repo-only assets, or generated files required.
- **Lockstep versioning:** `crates/tapwing/Cargo.toml` and `packages/tapwing_driver/pubspec.yaml` always share the same version (`X.Y.Z`) so every release tag `vX.Y.Z` matches the `ref` written by `tapwing init`. Bump both files together before tagging; `cargo test --test init` fails if they drift.

---

## Black-Box Contract

`tapwing_driver` runs inside the app process strictly for speed and deterministic synchronization, while keeping tests **black-box** (equivalent to Maestro 2.12.1):

- **Read-only observation:** reads the Flutter semantics tree (`SemanticsBinding.ensureSemantics()`), platform brightness (`PlatformDispatcher.platformBrightness`), bottom keyboard insets (`FlutterView.viewInsets.bottom`), scheduler state (`hasScheduledFrame`, `transientCallbackCount`), public animating widgets (`AnimatedWidget`, `DualTransitionBuilder`, `FadeTransition`, `SliverFadeTransition`, `ImplicitlyAnimatedWidgetState`), active scrolling (`ScrollableState.position.isScrollingNotifier`), `dart:io` HTTP requests (`HttpOverrides`), pending non-system `BinaryMessenger` replies (`ChannelTracker`), and `ImageCache.pendingImageCount`. Note: inspecting `ImplicitlyAnimatedWidgetState.animation` and `ScrollableState.position.isScrollingNotifier` observes public framework widget state solely for settle detection (read-only, never mutates state).
- **Engine-boundary injection only:**
  - Taps, long presses (3000 ms), repeated taps, swipes, and scrolls enter through `PlatformDispatcher.onPointerDataPacket` (physical pixels), passing through hit testing, modal barriers, and the gesture arena. Accelerated drags (`swipe`, `scroll`, `scrollUntilVisible`) emit synthetic pointer timestamps spanning the full logical `duration` (using a monotonic high-water mark clock that naturally returns to wall time once caught up) so Flutter's `VelocityTracker` computes the exact same fling velocity as Maestro's real-time `input swipe` without blocking wall clock.
  - Text, IME actions (`Enter`), and backspace enter through `ui.channelBuffers.push` (`TextInputClient.updateEditingState`, or `TextInputClient.updateEditingStateWithDeltas` when `enableDeltaModel: true`, and `TextInputClient.performAction`) and are mirrored to the native IME via `TextInput.setEditingState`, so `TextInputFormatter`s, `onChanged`, and `onSubmitted` execute normally. In default Maestro-parity mode, `inputText`/`eraseText` with no focused client succeed with a warning (`ime.noClient: true`); opt-in `strictInput: true` (`--strict-input`) fails with `NO_FOCUS`.
  - In-app hardware keys (`Tab`, `Escape`, and unfocused `Enter`/`Backspace`) enter through `PlatformDispatcher.onKeyData` (`synthesized: true`), passing through Flutter's `HardwareKeyboard` and focus traversal.
  - System back (`back` / `pressKey: BACK`) mirrors Android `KEYCODE_BACK`: when `viewInsets.bottom > 0`, dismisses the soft keyboard via `TextInput.hide` (sent to the engine like the framework does) without popping the route; when `viewInsets.bottom == 0`, delivers `popRoute` on `flutter/navigation` via `ui.channelBuffers.push`. `hideKeyboard` mirrors Maestro's Android behavior: hides the keyboard when visible, or sends `popRoute` with a warning when no soft keyboard is visible.
- **Platform semantics (`DriverPlatform`):** the driver reproduces Maestro's Android *or* iOS driver, picked from the running platform and confirmed by the CLI via `configure` (`platform`):
  - **Android** (`AndroidDriver`): hierarchy composed like Flutter's Android `AccessibilityBridge`; geometry (bounds, `width`/`height`, absolute points, swipe coordinates) in **physical pixels**.
  - **iOS Simulator** (`IOSDriver.mapViewHierarchy`): hierarchy composed like Flutter's iOS `SemanticsObject`s as XCUITest reports them — `accessibilityText` = label (+ `"\n" + tooltip`), `text` = value (toggles/checkboxes outside radio groups become switches with `"1"`/`"0"`, the only nodes marked `checked`), `hintText` always empty, `resource-id` = identifier, `focused` always `false`, no `clickable` (so no `clickableFirst` reordering), plus an optional root application element (`appName`, passed by the CLI from `CFBundleDisplayName`). Geometry is in **whole logical points** (truncated like `AXFrame.boundsString`) for snapshots, selectors, point parsing, and swipe/scroll math; `DriverContext.tap`/`drag` convert to physical pixels for `PointerDataPacket`s.
  - iOS commands: `back` and `pressKey` `BACK`/`TAB`/`ESCAPE` are no-ops (`IOSDriver.backPress`/`pressKey`); `hideKeyboard` sends `TextInput.hide` and succeeds, warning when an editable field stays focused (stock Maestro iOS fails there with Flutter); opt-in `strictHideKeyboard` (`--maestro-compat=strict`) emulates Maestro's two 50 ms center micro-swipes and fails with Maestro's message if the field is still focused.
- **Never** invokes widget callbacks (`onTap`, `onPressed`), `SemanticsAction`s, `ScrollController.jumpTo`, or app state/DI directly.
- **Zero third-party runtime dependencies:** depends exclusively on the Flutter SDK.

---

## Architecture & Module Layout

```text
lib/
├── tapwing_driver.dart                 Public API barrel (TapwingBinding, TapwingBindingMixin)
└── src/
    ├── binding.dart                    Binding mixin & singleton wiring all subsystems
    ├── platform.dart                   DriverPlatform (Maestro Android vs iOS semantics)
    ├── snapshot/
    │   └── snapshot.dart               Semantics tree -> KNode / Snapshot + allocation-free hash
    ├── selector/
    │   ├── maestro_regex.dart          Maestro 2.12.1 full-match regex + literal fallback
    │   └── selector.dart               Selector model & Maestro filter pipeline (resolve/resolveAll)
    ├── idle/
    │   ├── animation_scanner.dart      Significant (>= 2 % screen) running animation scanner
    │   ├── channel_tracker.dart        Pending non-system BinaryMessenger reply tracker (with safety cap)
    │   ├── http_tracker.dart           HttpOverrides wrapper tracking in-flight dart:io requests (+ F8b request log)
    │   ├── keyboard_waiter.dart        Soft-keyboard viewInsets transition wait & deduplication
    │   ├── semantics_motion_probe.dart How far the semantics tree moved between two frames (read-only)
    │   └── idle_detector.dart          SettlePolicy, BusyReport, frame/loading/animation settle loop
    ├── input/
    │   ├── pointer_input.dart          Engine-boundary tap, drag, and back injection
    │   └── text_input.dart             flutter/textinput observer, IME injector (plain + delta), BinaryMessenger wrapper
    ├── diagnostics/
    │   ├── app_issues.dart             F7 AppIssue model, classification, dedup ring buffer & delivery
    │   ├── error_hooks.dart            F7 chained FlutterError / PlatformDispatcher hooks + overflow scan
    │   ├── http_log.dart               F8b HTTP request log ring buffer & privacy redaction
    │   ├── overflow_detector.dart      Profile/release layout overflow detector (debug-report parity)
    │   └── text_input_log.dart         300-entry IME/tap ring buffer & ImeDiagnostics payload
    ├── protocol/
    │   ├── rpc_error.dart              DriverError & wire error codes
    │   ├── rpc_messages.dart           Typed request/response/hello envelopes, params, and results
    │   └── rpc_transport.dart          RpcTransport interface & SocketRpcTransport framing
    └── server/
        ├── driver_context.dart         Shared command execution context, reactive waits, point parser
        ├── command_handlers.dart       CommandDispatcher & dedicated per-RPC-method handlers
        ├── rpc_session.dart            Per-connection newline-delimited JSON RPC loop
        └── tapwing_server.dart         Loopback TCP dial-out & reconnect loop (adb reverse / port file)
```

---

## Request Flow

1. **Connection:** Before every dial, `TapwingServer.resolvePort` picks the port: `TAPWING_PORT` from the environment, else `<tmp>/tapwing_port` (iOS simulator: `Platform.environment` is empty in Flutter on iOS, so the CLI writes its per-session host port into the app's data container before launching or attaching; the simulator shares the host loopback), else `7781` (override via `--dart-define=TAPWING_PORT=...`), which the Rust CLI reverses onto a per-device host port via `adb reverse` on Android.
2. **Handshake:** `RpcSession` immediately writes a `hello` event containing protocol version, build mode, PID, Dart version, screen size, DPR, and platform.
3. **Serialized dispatch & heartbeats:** Incoming newline-delimited JSON lines are decoded into `RpcRequest` and executed strictly one at a time on a per-connection queue by `CommandDispatcher`. While any handler runs, `RpcSession` emits `{"event":"progress","id":...}` every 2 s so the Rust CLI can enforce a short 10 s per-read timeout without dropping legitimate long commands; disconnecting cancels lingering wait loops promptly. Supported RPC methods:
   - Lifecycle / diagnostics: `ping`, `configure` (`settlePolicy`, `softKeyboard`, `strictInput`, `platform`, `appName`, `strictHideKeyboard`, `appIssues`, `httpLog`, `settleLoadTimeoutMs`, `settleHttpIgnore`; replies with `pixelsPerUnit`), `busy`, `snapshot`, `waitIdle`, `darkMode`, `appIssues` (`since`; returns `issues`, `latestSeq`, `dropped`), `httpLog` (`since`; returns `entries`, `inFlight`, `latestSeq`, `dropped`).
   - Lookups & text extraction: `assertVisible`, `assertNotVisible`, `copyTextFrom`.
   - Gestures & scrolling: `tapOn` (element-relative `relativePoint`, `longPress`, `repeat`/`delayMs`, `retryIfNoChange` respecting `kDoubleTapTimeout`, `waitUntilVisible`, `settleTimeoutMs`), `tapPoint`, `swipe` (coordinates, percentages, or element `selector` + `direction` + `relativePoint`), `scrollUntilVisible` (all 4 directions, `durationMs` from `speed`, `visibilityPercentage`, `centerElement`, `settleTimeoutMs`).
   - Text & keys: `inputText`, `eraseText`, `hideKeyboard`, `back`, `pressKey` (`ENTER`, `BACKSPACE`, `TAB`, `BACK`, `ESCAPE`; iOS: only `ENTER`/`BACKSPACE`, the rest are no-ops).
4. **Selectors:** Full Maestro 2.12.1 `Filters.kt` pipeline: `text`/`id` full-match regex with literal and newline fallback (matching `text`, `hintText`, `accessibilityText`, and `error` — note `KNode.error` is always `null` for pure Flutter UI since Flutter engine 3.47.5 only populates it in `AccessibilityViewEmbedder` for platform views; retained for future native-lane parity), `traits` (`text`, `square`, `long-text`), `css` (no-op empty match on Android matching `Driver.queryOnDeviceElements`), `enabled`/`checked`/`focused`/`selected`, `width`/`height`/`tolerance`, `below`/`above`/`leftOf`/`rightOf`, `childOf`/`containsChild`/`containsDescendants`, visibility filtering (`>= 10%`), `deepestMatchingElement`, `clickableFirst`, and `index` sorting.
5. **Action lifecycle (e.g. `tapOn`):**
   - Reactively waits on rendered frames (gating `Snapshot.capture()` + `resolve()` on allocation-free `Snapshot.computeHash()` changes) until `Selector` resolves (17 s default lookup timeout, 7 s when optional, adjusted from the last mutating command by the host CLI).
   - Settles the UI and re-aims with refreshed bounds (matching the original semantics node id, else re-resolving, else falling back to the original node) so taps never land on a mid-transition coordinate.
   - Resolves `relativePoint` (element-relative `%` or pixel offset) if given, otherwise taps the element center.
   - Injects `PointerDataPacket` touch events via `PointerInput`, yields a zero-duration timer so focus microtasks dispatch `TextInput.show`, runs the settle detector, compares allocation-free semantics hashes, and optionally retries (`retryIfNoChange`, waiting past `kDoubleTapTimeout` before retrying single taps) or waits for center occlusion to clear (`waitUntilVisible`).
   - Replies with `{"id", "result", "us"}` (or `{"id", "error"}` on `DriverError` / unexpected exceptions), plus `issues` when app health capture is enabled (see below).

---

## App Health Capture (F7)

The driver observes what the app reports while a flow runs, even when every assertion passes. Design and CLI side: [PLAN.md §8.1](../../PLAN.md#81-f7-app-health-diagnostics).

- **Sources:** `FlutterError.onError` (layout overflow, build/layout/paint errors, image load failures, other framework assertions) and `PlatformDispatcher.onError` (uncaught async errors). In profile and release builds, `OverflowDetector` runs after each settle, because Flutter compiles its overflow report out there ([spike](../../spikes/f7-profile-overflow/README.md)).
- **Read-only chaining:** every hook calls the handler installed before it, and `PlatformDispatcher.onError` returns the previous handler's result (`false` without one). Handlers replaced by the app (Crashlytics, Sentry) are re-wrapped after the first frame and at every RPC. An error passing two Tapwing wrappers in one chain is recorded once.
- **Delivery:** opt-in via `configure` `appIssues: true`. Each response then carries `issues` recorded since the previous response, each tagged with the `request` id that caused it. `appIssues` re-reads the buffer. `configure` `appIssues: false` uninstalls the hooks and clears the buffer.

## HTTP Request Log (F8b)

For `tapwing test --trace` ([PLAN.md §8.4](../../PLAN.md#84-f8b-trace-viewer)), `HttpTracker` also records the app's `dart:io` requests. It is off by default; with it off, the tracker only counts in-flight requests for idleness, as before.

- **Opt-in:** `configure` `httpLog: true`. Each response then carries `http` entries that finished since the previous response (method, redacted URL and headers, status, `startMs`/`headersMs`/`durationMs`, request/response bytes, sanitized error, `state`), tagged with the `request` id in flight when the request started. `httpLog` re-reads finished entries and lists requests still in flight. `configure` `httpLog: false` clears the log while keeping idle counting active.
- **Chaining:** an `HttpOverrides` the app installs after the binding is re-wrapped by `ensureInstalled()` (at the first frame, `drawFrame`, and every RPC boundary) so idle counting (and HTTP logging when enabled) delegates to it. A per-tracker client-creation guard prevents double-counting or double-logging when the app's override chains to the previous `HttpOverrides`.
- **Known gap (FFI HTTP clients):** `HttpTracker` observes `dart:io` `HttpClient` traffic via `HttpOverrides`. Native FFI/JNI HTTP clients such as `cupertino_http` (`NSURLSession`) and `cronet_http` (`Cronet`) bypass `dart:io` `HttpOverrides` and `BinaryMessenger`, so their requests are not tracked for idle waiting or `httpLog`.
- **Privacy:** bodies are never read. See `HttpRedaction` for the URL, query, header, and error rules. The ring buffer holds 500 finished entries, with a `droppedHttp` counter.

---

## Settle Algorithm

`IdleDetector.waitForIdle` determines when the UI is ready for the next command:

1. **First frame:** If `firstFrameRasterized` is `false`, waits for the first frame before checking idleness.
2. **Keyboard transitions (`KeyboardWaiter`):** If a `TextInput.show` / `TextInput.hide` transition occurred within the last second and has not yet been settled, waits up to 700 ms from the request (or 120 ms once a hardware-keyboard session is detected after 2 consecutive editable misses or hinted via `configure.softKeyboard: false`) for `viewInsets.bottom` to begin moving, then waits for 48 ms (~3 frames) of unchanged insets. As a safeguard against slow cold-starting IMEs, any late `viewInsets.bottom` change observed inside the main settle loop resets stability, clears the absent latch, and waits for insets to stabilize.
3. **Full idle:** When `framesIdle` (`!hasScheduledFrame && transientCallbackCount == 0 && schedulerPhase == idle`) and `loadingIdle` (`pendingChannels == 0`, and no HTTP request or pending image still blocks settle, see [Bounded loads](#bounded-loads)), yields a zero-duration timer, re-checks pending keyboard changes, inset stability, and idleness, clears animation tracking, and returns `'idle'`.
4. **Stable fallback for infinite animations:** When frames keep ticking (e.g. spinners, blinking cursors) and `loadingIdle` holds, computes an allocation-free semantics hash (`Snapshot.computeHash()`) after each rendered frame:
   - **`SettlePolicy.v2` (default):** requires **3** consecutive identical hashes (counter starts at `1` on change) **and** no non-infinite significant animation or active scroll (`ScrollableState.position.isScrollingNotifier`) covering `>= 2 %` of the screen. Static animations (`AlwaysStoppedAnimation`, stopped mid-range `AnimationController`s, and `CurvedAnimation`/`ProxyAnimation`/`Tween` wrappers around them) are ignored. The element tree is rescanned on every check (never cached) so animations inside newly covered `TickerMode(enabled: false)` routes stop blocking immediately. Any animation/scroll that keeps running continuously for `>= 1500 ms` with an unchanged semantics tree is classified `infinite: true` (non-blocking) until it stops animating, preventing endless large animations from hitting the 3 s settle timeout on every command.
   - **`SettlePolicy.legacy`:** requires **8** consecutive identical hashes without animation scanning.
5. **Loading wait:** When frames are idle but HTTP requests, non-system platform-channel calls (`ChannelTracker`, excluding `flutter/*` and capped at 10 s per call), or images are still pending, wakes immediately when `HttpTracker` or `ChannelTracker` drains to zero (or polls every 4 ms for pending images). The HTTP wait also wakes when the last blocking request reaches the load bound.

**Between the swipes of `scrollUntilVisible`** the settle may also return `'calm'` (it never does elsewhere). A long low-friction coast (iOS bouncing physics) moves the content by a few pixels per frame for over a second, so the rounded hash keeps changing and the wait runs into its cap after every swipe. `SemanticsMotionProbe` samples the semantics tree on each rendered frame and measures how far any node moved (wall clock, so refresh rate and dropped frames do not skew it); once the content has been seen moving faster than 300 logical px/s and has slowed below it for the stable frame count, with nothing but geometry changing (loads, keyboard and non-scroll animations are still waited for as usual), the next swipe may start. The pointer-down catches the list at once. The target is checked before every swipe as before; once it is found the full settle runs as before (including active scrolls) with the time the early exit left unused added to its timeout, and the target is checked again at rest before the step returns. Reads the semantics tree only; no scroll controller is read or driven.

### Bounded loads

Waiting for network and images is a feature, but a request that never ends (long-poll, streaming download, hung call) or an image that never resolves must not make every step wait for its whole settle timeout.

- **Bound (`configure` `settleLoadTimeoutMs`, default 5000, `0` = none):** an HTTP request counts as in flight for settle for at most this long after it was opened (`HttpTracker.settleBound`, one timer per request, cancelled when the request ends first). `ImageCache` exposes only a count, so pending images are aged from what `IdleDetector` sees: a higher count means a load just started, and once the count has not grown for the bound, the images pending at that moment stop blocking until they finish. An image that was already pending before the first check is aged from that check.
- **Default:** 5 s is longer than the default settle timeout of a step (3 s) and equal to the launch `waitIdle`, so with defaults no single settle returns earlier than without the bound; only later steps stop waiting for a request that is already older than 5 s.
- **Ignore list (`configure` `settleHttpIgnore`, list of regexes):** a request whose full URL matches any pattern (`RegExp.hasMatch`, unanchored) never counts. The list replaces the previous one; an invalid pattern fails `configure` with `INVALID_COMMAND` and changes nothing.
- **Still logged:** requests past the bound and ignored requests stay in the F8b request log until they really end. `busy` reports `httpInFlight` as the number of requests that block settle and `pendingImages` as the raw `ImageCache` count.
- **Negotiation:** both fields are optional (an older host leaves the defaults in place), the host sends them on every attach, and `configure` echoes `settleLoadTimeoutMs` and the number of `settleHttpIgnore` patterns so the host can tell an older driver apart.
- The 30 s / 10 s safety timeouts and the immediate release of `text/event-stream` responses and upgrades are unchanged.

# Historical Design Plan (Archive)

> Archived pre-release planning log preserved from `PLAN.md` (with machine-local environment notes removed). For current documentation, see [Architecture](../architecture.md), [Design Decisions](../decisions.md), [Roadmap](../roadmap.md), and [Strategy](../../STRATEGY.md).

# Tapwing: Flutter-First Black-Box Runner for Maestro YAML Flows

> Working name: `tapwing`. Status (2026-11-21):
> - **F0 complete.** Emulator: 7.8× faster end-to-end, 10.0× excluding `launchApp`. Physical Pixel 8a: 11.4× end-to-end, 11.9× excluding `launchApp`. 0 premature settles, UI trees match Maestro down to the pixel (at both 560 and 420 dpi). See [bench/F0_RESULTS.md](../../bench/F0_RESULTS.md).
> - **F1 complete.** Parser produces identical IR to Maestro 2.12.1 across 463/463 fixtures from Maestro's repo and 81 custom edge-case probes. `tapwing check` validates 544 files in ~33 ms. See [compat/README.md](../../compat/README.md).
> - **F2 complete.** Full Flutter driver lane matching Maestro 2.12.1: 1:1 selectors with `Filters.kt` including `traits` and `css`, element-relative `tapOn`/`swipe`, `retryTapIfNoChange`, `waitUntilVisible`, `scroll`/`scrollUntilVisible` in all directions, `copyTextFrom`/`pasteText`/`setClipboard`, `inputRandom*`, `pressKey`, `takeScreenshot`/`assertScreenshot` with `cropOn` and exact pixel diff, `assertDarkMode`/`assertLightMode`. Verified with 45 Dart tests, 18 Rust tests (including `compat` 463/463 + 81/81), and 5/5 differential flows against Maestro 2.12.1 on the emulator (0 settle violations).
> - **F2.1 complete.** Fixes from F2 review (`back`/`hideKeyboard` parity with soft keyboard, 17 s timeouts + `adjustedToLatestInteraction`, `cropOn` capture order, default `permissions: all: allow` and `killApp` = `am kill`, screenshot paths in `--test-output-dir`, silent unfocused `inputText`/`eraseText` + `--strict-input`, `assertDarkMode` via `cmd uimode night`, `progress` heartbeats and queue cancellation on reconnect) and speedups (accelerated `swipe`/`scroll` with virtual timestamps preserving fling velocity, coasting scroll detection and large endless animations > 1.5 s, adaptive `KeyboardWaiter`, merged `launchApp` with PID verification, parallel CLI setup/teardown). Emulator benchmark: excluding `launchApp` 5.18 s (8.7×), end-to-end 7.2 s (7.3×), 0 settle violations; 53 Dart + 22 Rust tests + 7/7 positive and 2/2 negative differential flows (see [bench/F0_RESULTS.md](../../bench/F0_RESULTS.md#f21) and [bench/results/f2-differential/summary.md](../../bench/results/f2-differential/summary.md)).
> - **F3 complete.** Full Maestro 2.12.1 orchestrator (`Orchestra`) + embedded QuickJS-ng (`rquickjs`) JS engine over a clean `FlowDriver` trait (`AndroidFlutterBackend` + in-memory `FakeDriver`). Supports `runFlow` (file/inline, `when`, scoped `env`, sub-flow hooks), `repeat` (`times`/`while`), `retry` (max 3, Maestro-error-only retries), `runScript`, `evalScript`, `assertTrue`, `onFlowStart`/`onFlowComplete` hooks, `defineVariables`, dynamic `${…}` evaluation, `output`, `maestro.copiedText`/`platform`, `json()`, `relativePoint()`, native Rust `faker`, and synchronous pooled `http.*` (including `multipartForm`). Built with lazy QuickJS initialization + zero-JS fast paths, compiled expression/script caching, lightweight delta env scope stack, and persistent HTTP keep-alive. Verified with **195 Rust tests** (28 unit + 2 compat [463/463 + 81/81] + 165 orchestrator integration tests porting 160 applicable Maestro `IntegrationTest.kt` cases over `FakeDriver`).
> - **iOS spike complete (simulator).**
>   - The driver runs on the iOS simulator without modifications. The F2.1 driver passes the iOS benchmark flow 3/3 in ~13.1 s excluding `launchApp` (Maestro 43.5 s), 0 settle violations.
>   - Root cause of Maestro's `hideKeyboard` failure with Flutter on iOS is documented.
>   - iOS Simulator architecture and `hideKeyboard` parity notes are captured below and in [../../bench/IOS_RESULTS.md](../../bench/IOS_RESULTS.md).
> - **iOS Simulator phase complete** (merged with F3 on `main`).
>   - CLI device abstraction (`crates/tapwing/src/device`): Android over adb, iOS Simulator over `simctl` (install/reinstall, launch with port file + PID verification, terminate, `clearState`, screenshots, `privacy`, `openurl`, `location`, `ui appearance`, keychain), platform picked from `--device` or `--platform`; simulator hardware-keyboard state passed to the driver.
>   - Driver reproduces `IOSDriver`: hierarchy composition and whole-point geometry for snapshots, selectors and swipes (pointer packets stay physical), app root element, no-op `back`, `pressKey` mapping, `hideKeyboard` with compatibility warning plus `--maestro-compat=strict`.
>   - `maestro hierarchy` parity on 10/10 screens (98/98 elements, exact point bounds). Benchmark flow 3/3 in both tools, 0 settle violations, **4.1×** wall clock and **4.0×** excluding `launchApp` (debug builds). 10/10 positive and 2/2 negative differential flows agree with Maestro. See [bench/IOS_RESULTS.md](../../bench/IOS_RESULTS.md).
> - **F4 complete.** Pure-Rust ADB smartsocket & emulator console client (`adb_client.rs`, streaming `sync:` `SEND`/`RECV` + atomic `.part` pulls + early `FAIL` recovery) and full Android + iOS Simulator host commands (`setPermissions`, `openLink`, `setLocation`/`travel`, `setOrientation`, `setDarkMode`/`toggleDarkMode`, `setAirplaneMode`/`toggleAirplaneMode`, `addMedia`, `startRecording`/`stopRecording`).
> - **F5 complete (Android UiAutomator + iOS Simulator XCTest + F5c hardening).**
>   - **Android Native Lane:** Maestro UiAutomator gRPC (`tonic`/`prost`), hash-cached APK installer, background warm-up, hybrid `--input auto|flutter|native` routing, full Unicode `inputText` via Maestro's Unicode IME (`ImeGuard`), typed `launchArguments`, screenshot-based `waitForAnimationToEnd` (99.5% similarity), and automatic per-app black-box fallback. Real Android permission dialog passes in ~4.1 s (`auto`, 0 settle violations) vs Maestro ~18.9 s; Czech Unicode native input passes in 248 ms.
>   - **iOS Simulator Native Lane (F5b):** Maestro XCTest HTTP driver (`maestro-ios-driver.jar`, persistent HTTP/1.1 keep-alive), lazy zero-overhead `IosNativeSlot` (pure-Flutter iOS flows pay 0 ms / never spawn `xcodebuild`), real SpringBoard permission alerts, `setOrientation`, `pressKey: Home|Lock`, and automatic black-box fallback. Verified live on `iPhone 18 Pro` (`ios/f5_permission_dialog.yaml` passes in `auto` [hybrid] & `native`; `ios/login_and_browse.yaml` stays 100% pure Flutter with 0 native calls).
> - **F6 complete (Workspaces, Sharding, JUnit/HTML Reports, `platform.android.disableAnimations` & CLI Parity).**
>   - Workspace execution planner (`config.yaml`, `flows` globs with `!` negation, `includeTags`/`excludeTags`, `executionOrder.flowsOrder` + `continueOnFailure`, `testOutputDir`) matching 42/42 Maestro 2.12.1 planner cases.
>   - Multi-device parallel sharding (`--shard-split <N>`, `--shard-all <N>`, deprecated `-s`/`--shards <N>`, comma-separated `--device`/`--udid`, `-p`/`--platform`, per-shard `shard-N/` artifacts, `--debug-output`, `--flatten-debug-output`, `MAESTRO_SHARD_INDEX`/`MAESTRO_SHARD_ID`).
>   - Maestro-compatible JUnit XML (`--format junit`), self-contained zero-CDN HTML & HTML-Detailed (`--format html|html-detailed`), `--output`, `--test-suite-name`, and multi-shard JSON (`--report`).
>   - `platform.android.disableAnimations` over pure-Rust ADB smartsocket with concurrent session setup and RAII restoration (`AnimationScaleGuard`).
>   - **Test suite after F6:** 367 Rust tests + 62 Dart tests green.
> - **F7 complete (App Health Diagnostics).** The driver captures Flutter framework errors, uncaught Dart errors, and layout overflows (including in profile builds via `OverflowDetector`). The CLI attributes them to the causing step by request id, applies `--app-errors report|fail|off` / `--app-errors-ignore` / `--app-errors-screenshot` (or `config.yaml` `tapwing:`), names app crashes with a `logcat` / `.ips` excerpt, surfaces dropped counts from its O(1) 500-entry `LinkedHashMap` ring buffer (`droppedIssues`), and adds them to `run.json`, JUnit `<system-err>`, and HTML. Verified on `emulator-5554` (profile), `iPhone 18 Pro` (debug), and physical **Pixel 8a** (Android 16 / API 36, profile): `f7_app_health.yaml` passes in Maestro 2.12.1 and Tapwing (`report`), fails at the expected step under `fail`; `negative/f7_app_crash.yaml` detects the Java crash in 95.3 ms; `f7_error_flood.yaml` sustains 2 695 errors/s with exact ring-buffer bounding and deduplication. See [§8.1](#81-f7-app-health-diagnostics).
> - **F8 complete (Failure Diagnostics & Trace Viewer).** F8a: "Did you mean" suggestions, pruned UI tree, and failure screenshot in console, JSON, HTML, and JUnit. F8b: `--trace` writes one offline, XSS-safe `trace.html` + `trace.json` per shard (steps, lanes, retry attempts, app issues, screenshots, UI trees) with a privacy-redacted HTTP request log from the driver's `HttpTracker`, at no measurable per-step cost (+0.35 ms on the iOS Simulator, −3.7 ms on an Android emulator). See [§8.4](#84-f8b-trace-viewer).
> - **Real-project pilot complete.** First run on a production Flutter app (Flutter 3.41.6, flavors, Auth0 in a Chrome Custom Tab, Firebase/Crashlytics) without touching its production code. Same results as Maestro 2.12.1 on every flow, one content flow 27 s → ~9 s, another 48 s → ~10 s. Fixed what it found: Android `back` is now a real `KEYCODE_BACK`, hellos from another platform are rejected, the driver supports Flutter ≥ 3.41. See [§8.2](#82-real-project-pilot).
> - **`tapwing init` & git-consumable driver complete.** `tapwing init [PROJECT_DIR]` wires `tapwing_driver` into `pubspec.yaml` (text-preserving, git `ref: v<version>` or `--driver-path`) and generates `lib/main_tapwing.dart` (pub workspaces, sync/async/args `main`, `--target`/`--output`, `--dry-run`, `--force`, `--pub-get`, Android flavor hints); CLI and driver versions stay lockstep-synced and tested.
> - **Distribution & F11 complete (Maestro native drivers removed).** `scripts/dist.sh` builds `dist/tapwing-<version>-<target>.tar.gz` (+ `.sha256`, **9.3 MB** for `aarch64-apple-darwin`, down from 24 MB) bundling only Tapwing's own Android + iOS Simulator native-lane drivers under `share/tapwing/drivers/` (no Maven Central downloads; `.sha256` sidecars verified at runtime); `scripts/install.sh` installs, upgrades and uninstalls it. Maestro's native drivers were removed on 2026-11-22 after physical Pixel 8a (API 36) parity verification; `--native-driver` and `tapwing.nativeDriver` stay as deprecated no-ops. Verified on a fresh Pixel 8a (`f5_permission_dialog.yaml` passes in `auto` and `native` from both dev build and unpacked release tarball, zero `dev.mobile.*` packages) and fresh iOS Simulator. CI (`.github/workflows/ci.yml`), multi-target release workflow (`.github/workflows/release.yml` for `aarch64-apple-darwin`, `x86_64-apple-darwin`, `x86_64-unknown-linux-gnu` with secret-gated macOS signing/notarization), and offline packaging tests (`scripts/test-dist.sh`) are in place; `x86_64-apple-darwin` release build verified locally, while Linux build/CI execution awaits the first GitHub Actions run and macOS signing/notarization awaits `APPLE_*` secrets. See [§8.3](#83-f11-own-native-drivers) and [docs/install.md](../../docs/install.md).
>
> Date: 2026-11-22 · Reference version: Maestro **2.12.1** (source from `main`, 2026-11-17), Flutter **3.47.5** (Pixel 8a verified on Flutter **3.50.0**)
>
> **Scope: Android, plus iOS Simulator only.** Physical iOS devices will not be supported.

---

## 0. TL;DR

- **Black-box tests, gray-box driver.** Tests are external Maestro YAML files that only see what the user and accessibility see, with no access to app internals. The driver runs in-process (like Detox or Espresso) purely for speed and exact synchronization. See §2: this is **not Patrol**.
- **Rust host runner** (single binary): Maestro YAML parser, orchestrator, JS engine (QuickJS), Android control over ADB, reporting.
- **In-app driver** (`tapwing_driver`, dev dependency + dedicated entrypoint). Reads the Semantics tree and injects input **at the same engine → framework boundary the OS uses**.
- **Native lane** (UiAutomator, reusing Maestro's Android driver) warms up in the background and handles system dialogs, WebViews, and other apps.
- **Compatibility:** identical YAML syntax, timeouts, and matching semantics to Maestro 2.12.1. Verified by porting Maestro's 180 integration tests and running differential tests against real Maestro.

### Approved Decisions

| # | Decision |
|---|---|
| 1 | Driver as a **dev dependency** + entrypoint `lib/main_tapwing.dart`; production code untouched |
| 2 | **Android first** (emulators and physical devices over ADB), followed by **iOS Simulator** (no physical iOS devices) |
| 3 | Default input **in-process** (engine → framework boundary); `--input native` via UiAutomator as a switch |
| 4 | AI commands (`assertWithAI`, `assertNoDefectsWithAI`, `extractTextWithAI`) **not planned (dropped 2026-11-22, was post-MVP)**; they parse cleanly like Maestro 2.12.1 and fail at run time with a clear message. Maestro 2.12.1 runs them only through Maestro Cloud (`api.copilot.mobile.dev`, prompts on the server), so a local LLM provider could not give comparable verdicts |
| 5 | **App health diagnostics (F7)** come right after F6: the driver observes Flutter errors, uncaught Dart errors, and crashes during every run. They are reported by default and fail the run only when opted in (`--app-errors fail`), with no new YAML commands. See [§8.1](#81-f7-app-health-diagnostics) |

---

## 1. Research

### 1.1 Why Maestro Is Slow (from 2.12.1 Source)

| Mechanism | Location | Impact |
|---|---|---|
| JVM CLI; installs and starts an on-device driver APK via instrumentation at startup | `maestro-cli`, `maestro-android` | Seconds of overhead per run |
| Every lookup = **full view hierarchy dump** via UiAutomator, filtered on the host | `Maestro.viewHierarchy()`, `Filters.kt` | Hundreds of ms to seconds per query |
| Waiting for elements = polling loop `MaestroTimer.withTimeout` without backoff | `MaestroTimer.kt`, `Orchestra.kt` | Latency = multiples of dump cost |
| Default timeouts: lookup **17,000 ms**, optional **7,000 ms** | `Orchestra.kt:142-143` | `optional` / `assertNotVisible` up to 7–17 s |
| Post-action settle: `isWindowUpdating` loop + hierarchy dumps + **screenshot diff** (0.5% threshold) | `AndroidDriver.kt:716+`, `Maestro.kt:578, 796` | Hundreds of ms to seconds after every command |
| `retryTapIfNoChange` = screenshot before and after tap | `Maestro.kt:495` | Extra screenshots on every tap |
| Flutter is a black box to Maestro: reads the native mirror of Flutter's **Semantics tree** through the accessibility bridge | Maestro blog; `Semantics.identifier` since Flutter 3.19 | Double tree translation, update lag |

### 1.2 Key Compatibility Findings

On a Flutter app, Maestro **only sees the Flutter Semantics tree** (via Android's accessibility
bridge). Tapwing reads the **exact same tree directly in-process**, yielding the same element set
without the bridge overhead.

What must be replicated 1:1:
- `text` / `id` are regexes with **IGNORE_CASE + DOT_MATCHES_ALL + MULTILINE** (`Orchestra.kt:1904`), evaluated as **full match**. Fallback: literal equality and a variant with `\n` replaced by a space (`Filters.textMatches`). Invalid regexes fall back to literal matching (`toRegexSafe`).
- `text` matches against `text`, `hintText`, and `accessibilityText`.
- `index` sorts by `bounds.y`, then `bounds.x` (`Filters.INDEX_COMPARATOR`); negative indices count from the end.
- Defaults: `scrollUntilVisible` timeout 20,000 ms, speed 40, visibility 100%; `swipe` 400 ms; `assertScreenshot` threshold 95%.
- **Verified in F0:** how Flutter's Android `AccessibilityBridge` composes `label` / `value` / `hint` into `text` / `contentDescription` / `hintText`. Tapwing synthesizes identical strings so full-match regexes behave identically (verified differentially via `maestro hierarchy` vs `tapwing hierarchy`).

### 1.3 Prior Art

| Tool | How it works | What to take | Why it is not enough |
|---|---|---|---|
| **Patrol 4** | Dart tests via `integration_test`, native automation via UIAutomator | Hybrid Flutter + native architecture | White-box Dart tests, no YAML, see §2 |
| **Marionette** (LeanCode) | Binding mixin, VM service extensions, real pointer events | Mixin pattern for Sentry/etc. coexistence | Debug/profile only; self-described "not a CI regression tool" |
| **Fluttium** | YAML flows over Semantics tree | Confirms YAML + semantics works | Custom syntax, unmaintained |
| **Detox** (React Native) | "Gray-box": in-process driver for synchronization, external tests | **Exact model we want** | Not for Flutter |
| **Maestro Android driver** | gRPC (`maestro_android.proto`): viewHierarchy, tap, inputText, launchApp… | Reuse as native lane (Apache 2.0) | By itself, it is the slow path |

---

## 2. Black-Box, Not Patrol

### 2.1 Comparison

| | Maestro | **Tapwing** | Patrol |
|---|---|---|---|
| Where tests live | External YAML | **External YAML** | Dart code compiled into the app |
| Test edit requires rebuild | No | **No** | Yes (hot restart in dev mode) |
| What tests "see" | Accessibility tree | **Semantics tree (same as Maestro)** | Anything in Dart (widgets, state, DI, mocks) |
| Can tests touch app state | No | **No** | Yes |
| App changes required | None | Dev dependency + entrypoint, nothing else | Dev dependency + test target + Dart tests |
| Who writes tests | QA & devs | **QA & devs** | Devs |
| Same flows run in Maestro | – | **Yes** (guarantees no coupling to internals) | No |
| One build for all tests | Yes | **Yes** | Tests are part of the build |

### 2.2 Driver Contract

From the framework's perspective, the driver behaves like the Flutter engine: **it only reads what
the engine exposes to accessibility, and only injects what the engine delivers from the OS.**

**Allowed:**
- Read the Semantics tree (labels, values, flags, geometry) — i.e., what the user and screen reader see.
- Inject input **at the engine → framework boundary**:
  - Touches: `PointerDataPacket` via `PlatformDispatcher.onPointerDataPacket` (physical px, same path as a finger; goes through hit testing and the gesture arena; accelerated drags in `swipe`/`scroll`/`scrollUntilVisible` carry synthetic timestamps spanning the full logical duration via a high-water-mark clock, so `VelocityTracker` computes the same fling velocity as Maestro without waiting in wall-clock time).
  - Text: `flutter/textinput` platform messages via `channelBuffers.push` (same as IME); hiding the keyboard sends `TextInput.hide` to the engine.
  - Back: `flutter/navigation` (`popRoute`), matching the system back button (when the soft keyboard is visible, sends `TextInput.hide` only, matching Android IME).
  - Keys: key data matching hardware keyboard events.
- **Observe** app idleness: frames, animations, in-flight HTTP, image loading. Read-only observation, never controlling app logic.

**Forbidden:**
- Calling `onPressed` / `onTap` or `SemanticsAction` directly. If a dialog covers a button, the tap hits the dialog just like a real finger.
- Reading or mutating widget state, providers, blocs, or DI containers.
- Scrolling a `ScrollController` directly (`scrollUntilVisible` performs real drag gestures).
- Mocking anything inside the app.

Anything violating this contract is **opt-in, disabled by default, and flagged in the report**
("this run was not strictly black-box"):
- `id` matching `ValueKey<String>` as well (default: `Semantics.identifier` only, matching Maestro)
- App backdoors (`tapwing.app.call(...)` from JS)
- Turbo mode: accelerated animations (`timeDilation`), scrolling via `jumpTo`

The contract is enforced by testing: CI verifies every suite flow also passes in Maestro
(differential run), and the driver in `strict` mode rejects opt-in APIs.

### 2.3 Note on "Gray-Box"

Placing the driver inside the app process is a deliberate trade-off. It is the only way to know
deterministically when the UI has settled without polling or screenshot diffs, which is the primary
source of both speed and stability. Test logic stays external and has no more visibility than a real
user. For pure zero-instrumentation black-box runs, the **native lane fallback mode** remains
available (slower, fully compatible; engaged automatically if no handshake arrives).

---

## 3. Architecture (Android)

```
┌──────────────────── HOST ─────────────────────┐             ┌──────────────── ANDROID DEVICE ───────────────────────┐
│ tapwing (Rust, single binary)                 │             │ Flutter app (profile / release / debug)               │
│                                               │             │  └─ tapwing_driver (Dart, binding mixin)              │
│  yaml ──► spanned AST ──► Command IR          │  TCP/JSON   │      • snapshot: Semantics tree (read-only)           │
│  orchestrator (tokio)  ◄─────────────────────►│◄───────────►│      • selector engine (Maestro semantics)            │
│   • runFlow/repeat/retry/when/hooks           │ adb reverse │      • reactive waits on frame callbacks              │
│   • Maestro timeouts & optional semantics     │             │      • idle detector (frames, tickers, HTTP, images)  │
│  JS: rquickjs (output, maestro, http, json)   │             │      • input at engine → framework boundary           │
│  ADB: persistent connection (adb protocol)    │             │                                                       │
│  native lane (background warm-up) ───────────►│◄───────────►│ Maestro Android driver (UiAutomator, instrumentation) │
│  reports: JUnit (compatible), HTML, JSON      │ adb forward │   permission dialogs, WebView, other apps             │
└───────────────────────────────────────────────┘    gRPC     └───────────────────────────────────────────────────────┘
```

Orchestration stays on the host. Each UI command is a **single round-trip**: waiting and retries
execute inside the app rather than polling over ADB.

---

## 4. Why It Is Fast

| Maestro | Tapwing |
|---|---|
| Full view hierarchy dump via UiAutomator, filtered on host | In-process selector over live Semantics tree (target < 1 ms) |
| Polling loop until timeout | **Reactive waiting:** predicate re-evaluates after each rendered frame, resolves on the first matching frame |
| Settle via `isWindowUpdating` + screenshot diff | **Deterministic idle detector:** `!hasScheduledFrame && transientCallbackCount == 0`, no in-flight HTTP (`HttpOverrides`), `imageCache.pendingImageCount == 0` (a single request or pending image blocks for at most `--settle-load-timeout-ms`, 5 s by default; URLs matching `--settle-http-ignore` never do). Infinite animations (spinners) are handled via **layout/semantics hash** stability across frames plus significant-animation scanning |
| Tap via UiAutomator / `input` | `PointerDataPacket` straight into the framework, coordinates from `RenderBox`, zero OS latency |
| `inputText` via IME / adb character by character | `flutter/textinput` messages; formatters and `onChanged` run normally |
| `retryTapIfNoChange` via screenshots | "Did the tap have an effect?" = semantics tree hash changed or frame scheduled |
| JVM startup + driver APK install per run | Native binary; driver compiled into test build; native lane installed once (hash-cached) and warmed up in background, never on the critical path |
| Each `adb` call spawns a new process | Persistent socket to ADB server (`adb_client`), shell commands without forking |

**Target metrics** (hypotheses verified in the F0 benchmark):

| Operation | Target |
|---|---|
| CLI startup | < 50 ms |
| Attach to running app | < 300 ms |
| `assertVisible` on visible element | < 5 ms |
| `tapOn` + settle (no transition animation) | < 50 ms |
| `inputText` (20 chars) | < 20 ms |
| Typical 30-step flow | **≥ 10× faster** than Maestro (primary KPI) |

---

## 5. Maestro YAML Compatibility

### 5.1 Levels

1. **L1 parse:** every valid Maestro flow parses into the exact same IR, including shorthand forms (`- tapOn: Login`, `- runFlow: file.yaml`, `- launchApp`), case-insensitive enums, scalar coercion (`times: "3"` and `3`), `label`, `optional`, front matter before `---`, and `${}` expressions. Errors report exact line and column.
2. **L2 behavior:** identical pass/fail outcomes, timeouts, and semantics for `optional` / `when` / `retry` (max 3) / `onFlowStart` and `onFlowComplete` hooks.
3. **L3 outputs:** compatible JUnit XML structure, exit codes, artifacts (`testOutputDir`), CLI flags `test`, `--device`, `-e`, `--include-tags`, `--exclude-tags`, `--format`, `--output`, `--shard-split`, `--shard-all`, `--config`, `--debug-output`, `--test-output-dir`.

### 5.2 Command Coverage (Maestro 2.12.1, `YamlFluentCommand.kt`), Android

| Lane | Commands |
|---|---|
| **Flutter driver** | `tapOn`, `doubleTapOn`, `longPressOn`, `assertVisible`, `assertNotVisible`, `extendedWaitUntil`, `waitForAnimationToEnd`, `scroll`, `scrollUntilVisible`, `swipe` (all 4 variants), `inputText`, `inputRandom{Text,Number,Email,PersonName,CityName,CountryName,ColorName}`, `eraseText`, `copyTextFrom`, `pasteText`, `hideKeyboard`, `back`, `pressKey` (enter, backspace, tab), `takeScreenshot` and `assertScreenshot` (whole screen via `screencap` for status-bar parity; optional Flutter layer), `assertDarkMode`, `assertLightMode` |
| **ADB / host** | `launchApp` (`am start -W`; clearState = `pm clear`; permissions = `pm grant/revoke`; arguments = intent extras), `stopApp` (`am force-stop`), `killApp`, `clearState`, `setPermissions`, `openLink` (`am start -a VIEW -d`), `setLocation` and `travel` (emulator `geo fix`, mock provider via native lane on physical devices), `setOrientation`, `setAirplaneMode`, `toggleAirplaneMode` (`cmd connectivity airplane-mode`), `setDarkMode`, `toggleDarkMode` (`cmd uimode night`), `setClipboard`, `addMedia` (push + media scan), `startRecording`, `stopRecording` (`screenrecord`, chained past 3-minute limit), `pressKey` (home, volume, power…) |
| **Orchestrator** | `runFlow` (file / commands / when / env), `runScript`, `evalScript`, `assertTrue`, `repeat` (times / while), `retry`, conditions `platform` / `visible` / `notVisible` / `true` |
| **Native lane** | Anything outside Flutter: system permission dialogs, WebViews, platform views, notifications, other apps; plus full black-box fallback |
| **No-op on Android** | `clearKeychain` (iOS-only, matches Maestro on Android) |
| **Not planned** | `assertWithAI`, `assertNoDefectsWithAI`, `extractTextWithAI` (parse-only, see decision 4); web-only `action` and `css` ignored with a warning, along with Cloud-only keys (`notifications`, `baselineBranch`) |

### 5.3 Selector Mapping to Flutter

| Maestro | Flutter source |
|---|---|
| `text` | Semantics `label`, `value`, `hint`, `tooltip`; strings composed identically to Android `AccessibilityBridge` (see §1.2) |
| `id` | `Semantics.identifier` (what Maestro sees as `resource-id`) |
| `enabled`, `checked`, `selected`, `focused` | `SemanticsFlag`s |
| `below`, `above`, `leftOf`, `rightOf`, `childOf`, `containsChild`, `containsDescendants` | Semantics tree geometry and structure |
| `width`, `height`, `tolerance`, `point` | **Physical px** matching Maestro on Android (converted via `devicePixelRatio` and view offset); `%` relative to screen |
| `traits` | `text`, `long-text` (> 200 chars), `square` (< 3% aspect diff) |
| Visibility | Semantics tree already omits elements under modal routes (`BlockSemantics`), intersected with viewport |

Matching modes: `strict` (Maestro parity, default) and `lenient` (match any of label/value/hint; can
change `assertNotVisible` outcomes, therefore opt-in).

### 5.4 JavaScript

- Engine: **rquickjs** (QuickJS-ng). Boa is pure Rust, but significantly slower in benchmarks.
- Globals matching `GraalJsEngine.kt`: `output`, `maestro` (`copiedText`, `platform`…), env variables, `http` (synchronous get/post/put/delete/request), `json()`, `relativePoint`, `faker` (Datafaker subset).
- Opt-in extension (breaks black-box, see §2.2): `tapwing.app.call('seedUser', {...})`.

---

## 6. Tech Stack

**Rust (host)**
- `tokio`, `clap`
- YAML: `yaml-rust2` + custom typed decoder replicating Jackson semantics (`serde_yaml` is deprecated; handles YAML 1.1/1.2 quirks matching Maestro's SnakeYAML).
- Regex: `fancy-regex` (Maestro uses Java regex with lookaround and backreferences)
- `rquickjs`, `ureq` (persistent HTTP/1.1 keep-alive for JS `http.*` and iOS XCTest), `quick-xml`, `image` (screenshot diff), `zip`, `sha2`
- ADB: custom pure-Rust smartsocket & emulator console client (`src/device/adb_client.rs`, zero per-call `adb` subprocesses)
- Native lanes: `tonic` + `prost` over `maestro_android.proto` (Android UiAutomator) and `ureq` over Maestro's XCTest HTTP runner (iOS Simulator)

**Dart (`tapwing_driver`)**, depends only on `flutter`
- `TapwingBindingMixin` (applied last for Sentry/etc. coexistence), `wrapWithDefaultView` for root `RepaintBoundary`
- Reading: `SemanticsBinding.ensureSemantics()`, `SemanticsOwner`
- Input: `PlatformDispatcher.onPointerDataPacket`, `ServicesBinding.channelBuffers.push` (`flutter/textinput`, `flutter/navigation`)
- Idle: `SchedulerBinding` (`hasScheduledFrame`, `transientCallbackCount`), `HttpOverrides`, `imageCache.pendingImageCount`
- Optional `tapwing_http` adapters (dio, `package:http`) for non-`dart:io` clients (cronet)
- All APIs verified against local Flutter SDK 3.47.5.

**Transport:** the app dials out to the runner over `adb reverse tcp:7781 tcp:<host-port-per-device>`, giving each device its own host port so parallel shards never collide. Length-prefixed or newline-delimited JSON protocol: request/response + push events (lifecycle, progress heartbeats, logs, frame stats). Handshake carries protocol version, Flutter version, build mode, and PID. Works in **profile and release**; debug is for dev mode (always test performance against profile/release builds).

**App integration** (generated by `tapwing init`):
```dart
// lib/main_tapwing.dart
import 'package:tapwing_driver/tapwing_driver.dart';
import 'package:my_app/main.dart' as app;

void main() {
  TapwingBinding.ensureInitialized(); // must run before app.main()
  app.main();
}
```
Build: `flutter build apk --profile -t lib/main_tapwing.dart` (or `tapwing build`).

---

## 7. Run Modes

| Mode | Audience | How it works |
|---|---|---|
| `tapwing test` | Local runs, CI, regression | Prebuilt APK / simulator app with driver, exact Maestro semantics, sharding across N emulators/devices, rich failure diagnostics |
| Black-box fallback | Uninstrumented app | Everything routed via native lane; slower, fully compatible. Detected automatically |

---

## 8. Phases & Milestones (Android, then iOS Simulator)

Estimates are rough (1 developer + AI assistant).

| Phase | Scope | Acceptance Criterion |
|---|---|---|
| **F0 Spike + benchmark** (3 days) | Sample Flutter app (login, list, form, dialog, permission); Maestro 2.12 baseline on emulator; prototype driver (tap, assertVisible, inputText across engine boundary) + minimal Rust CLI over `adb reverse`; verify `AccessibilityBridge` string composition | **≥ 10× faster** per-command on the same flow; documented string mapping |
| **F1 Parser** (1 week) | Full command schema, spanned diagnostics, `tapwing check` | Parses all **177+ YAML fixtures** from `maestro-test` and e2e workspaces; IR matches Maestro (JSON dump via small Kotlin harness) |
| **F2 Dart driver** (2 weeks) | Binding mixin, snapshot, selector engine, idle detector, reactive waiting, actions, screenshots, `strict` contract | Unit + widget tests; selectors 1:1 with `Filters.kt` |
| **F3 Orchestrator + JS** (1.5 weeks) | Flow control, hooks, env, `${}`, `rquickjs` with globals, timeouts, `optional` | Ported `IntegrationTest.kt` (**~180 tests**) green against a Rust `FakeDriver` |
| **F4 ADB device layer** (1 week) — **done 2026-11-21** | Pure-Rust ADB smartsocket & emulator console client (`adb_client.rs`), lifecycle, `setPermissions`, `openLink`, `setLocation`/`travel`, `setOrientation`, `setDarkMode`/`toggleDarkMode`, `setAirplaneMode`/`toggleAirplaneMode`, `addMedia`, `startRecording`/`stopRecording` (Android + iOS Simulator) | All host commands verified on Android emulator (`f4_device_commands.yaml`, 25/25) and iOS Simulator (`ios/f4_device_commands.yaml`); 269 Rust tests + 62 Dart tests green |
| **MVP** = F0–F4 | ~**4–5 weeks** — **done 2026-11-21** | Real Flutter project runs on Tapwing instead of Maestro |
| **F5 Native lanes** (1–1.5 weeks) — **done 2026-11-21** | Android UiAutomator gRPC (`tonic`/`prost`), hash-cached APK installer, Unicode IME (`ImeGuard`), typed `launchArguments`, screenshot-based `waitForAnimationToEnd`; iOS Simulator XCTest HTTP (`maestro-ios-driver.jar`, lazy `IosNativeSlot`, SpringBoard alerts, `setOrientation`, `Home`/`Lock`); hybrid routing (`--input auto\|flutter\|native`) and automatic per-app black-box fallback | Real Android & iOS Simulator permission dialogs (`f5_permission_dialog.yaml`, `ios/f5_permission_dialog.yaml`) pass in `--input auto` (hybrid) and `--input native`, Android Unicode IME passes live, and pure-Flutter flows keep 0 ms native-lane overhead (330 Rust + 62 Dart tests green) |
| **F6 Workspace + CLI parity** (1 week) — **done 2026-11-21** | `config.yaml` (`flows` globs incl. `!` negation, `includeTags`/`excludeTags`, `executionOrder`, `testOutputDir`, `platform.android.disableAnimations`), multi-device parallel sharding (`--shard-split`, `--shard-all`, `--shards`/`-s`, `--device`/`--udid`, `-p`/`--platform`, `--debug-output`, `--flatten-debug-output`), JUnit XML & self-contained HTML/HTML-Detailed reports (`--format`, `--output`, `--test-suite-name`, `--report`) | Existing Maestro CI pipeline switches by swapping the binary (42/42 planner cases + 367 Rust & 62 Dart tests green) |
| **F7 App Health Diagnostics** (1–1.5 weeks) — **done 2026-11-21** | Capture what the app reports while a flow runs, even when every assertion passes: Flutter framework errors (layout overflow, build/paint exceptions, image load failures), uncaught Dart errors, and app crashes. Attributed to the step that caused them, deduplicated, reported in console + `run.json`; `--app-errors report\|fail\|off`. Design in [§8.1](#81-f7-app-health-diagnostics) | See [§8.1 acceptance criteria](#acceptance-criteria) |
| **Pilot: real project** (1 day) — **done 2026-11-21** | A production Flutter app, dev flavor, local only, in phases L0–L5. Design and results in [§8.2](#82-real-project-pilot) | Same pass/fail as Maestro on the project's core flows, production code untouched, every divergence explained |
| **F8 Failure Diagnostics & Trace Viewer** — **done 2026-11-22** (F8a + F8b; `tapwing dev` dropped) | **F8a (done):** rich failure reports on every non-optional failing step — "Did you mean" selector suggestions (state/flag mismatches, cross-attribute `text`/`id` hits, Maestro full-match substring hints, Levenshtein typos), pruned indented UI tree dump (Flutter & native lanes), automatic failure screenshot PNG + full hierarchy `.txt` in `<test-output-dir>/failures/`, inline console output, JSON `failure` object, HTML self-contained base64 screenshot + collapsible tree, and JUnit `<system-err>`.<br>**F8b (done):** trace viewer: `--trace` writes one offline `trace.html` + `trace.json` (`tapwing.trace/1`) per shard, built on the F7 step-event model (timing, lane, nesting, retry attempts, app issues, failure or per-step screenshots and UI trees), plus a privacy-redacted HTTP request log from `HttpTracker`. Design and results in [§8.4](#84-f8b-trace-viewer). | F8a: failing lookup reports actionable suggestions, pruned UI tree, and failure screenshot across console, JSON, HTML (zero external URLs), and JUnit without disturbing passing runs, `optional: true`, or intermediate `retry` attempts. **Met** (`f8_failure_reports.rs`). F8b: per-step HTTP attribution, redaction, offline XSS-safe viewer, zero cost without `--trace`. **Met** on the iOS Simulator and an Android emulator (+0.35 / −3.7 ms per step, ~28 KB HTML; [bench/results/f8b/summary.md](../../bench/results/f8b/summary.md)). |
| **F10 Hardening** (ongoing) | Differential tests against Maestro on real apps, multi-view, add-to-app, tracking new Maestro releases | |
| **iOS Simulator** (post-MVP, ~1.5 weeks) — **done 2026-11-21**, see [bench/IOS_RESULTS.md](../../bench/IOS_RESULTS.md) | Scope:<br>• CLI drives simulator via `simctl`: install, launch with port & PID, terminate, `clearState` via reinstall, screenshot, `privacy`, `openurl`, `location`, `ui appearance`, `addmedia`, `recordVideo`.<br>• Driver composes hierarchy like Maestro iOS: points instead of px, `label`/`title`/`value`/`placeholder`, app root element.<br>• Port handoff via `tmp/tapwing_port` since `Platform.environment` is empty on iOS.<br>• iOS semantics: `back` is a no-op; `hideKeyboard` succeeds with a compatibility warning, plus opt-in strict mode matching Maestro iOS.<br>• CLI passes simulator hardware-keyboard state to the driver to avoid waiting for a soft keyboard.<br>• iOS XCTest native lane (F5b) handles SpringBoard alerts, `setOrientation`, `pressKey: Home\|Lock`, and uninstrumented apps with 0 ms overhead on pure-Flutter runs. | Benchmark flow and F2/F5 flows pass in both Tapwing and Maestro on the simulator. Trees match `IOSDriver.mapViewHierarchy`. Physical iOS devices are not supported. **Met:** 3/3 + 10/10 + 2/2 negative + `ios/f5_permission_dialog.yaml` (`auto` & `native`), 98/98 elements identical. |
| **F11 Own native drivers** — **done 2026-11-22** (F11a + F11b + Maestro driver removal) | Replace and remove Maestro's on-device drivers, Tapwing's last runtime dependency on Maestro. Design and results in [§8.3](#83-f11-own-native-drivers).<br>• **F11a Android:** one Tapwing APK (`dev.tapwing.driver`, `drivers/android`) replacing `maestro-app.apk` + `maestro-server.apk`, same `maestro_android.proto` wire format, Unicode `inputText` without switching the IME.<br>• **F11b iOS Simulator:** own zero-dependency Swift + minimal ObjC XCUITest HTTP runner (`dev.tapwing.ios-runner`, `drivers/ios`) replacing `maestro-ios-driver.jar` after the AXe spike returned NO-GO (`spike/f11-ios-axe`). Same 14-route HTTP/1.1 keep-alive API, `IosNativeSlot::Lazy` preserved.<br>• **Maestro driver removal (2026-11-22):** removed Maestro APK/jar discovery, `ImeGuard`, `maestro-drivers.sha256`, `TAPWING_MAESTRO_*`, and Maven Central downloads in `scripts/dist.sh` (9.3 MB tarball); `--native-driver` and `tapwing.nativeDriver` stay as deprecated no-ops. | F11a: hierarchy parity on ≥ 8 screens, differential flows in `auto` and `native`, default IME unchanged — **Met** on `emulator-5620` (API 35) and physical **Pixel 8a** (Android 16 / API 36, 11/11 screens, 16/16 flows, mock location, fresh-install post-removal runs incl. release tarball; see [§8.3](#83-f11-own-native-drivers)). F11b: 14/14 routes over keep-alive, 0-diff hierarchy parity, Czech Unicode, all iOS differential flows pass in `auto` (0 native calls on pure-Flutter) & `native` incl. bundled release tarball — **Met** (see [§8.3](#83-f11-own-native-drivers)). |

### 8.1 F7: App Health Diagnostics

**Problem.** A Maestro flow only checks what its assertions name. A screen can show a `RenderFlex overflowed by 12 px` stripe, a failed image, or swallow an uncaught exception, and the flow still passes. Maestro cannot see any of this because it only reads the OS accessibility tree and screenshots. Tapwing's driver runs in-process, so it can **observe** what the framework reports. This is the first feature Maestro cannot offer, not just a faster equivalent.

**Contract fit (§2.2).** Observation only: every hook calls through to the handler that was installed before it, the app's own error UI (`ErrorWidget`, debug overflow stripes) still renders, and no app behavior changes. Flows stay plain Maestro YAML: **no new YAML commands**, so every flow still runs in Maestro unchanged. Policy lives in CLI flags and a separate `tapwing:` config section.

#### What is captured

| Kind | Source (driver) | Build modes |
|---|---|---|
| `layoutOverflow` | `FlutterError.onError`, `library: rendering library`, "overflowed" message (`RenderFlex`, `RenderConstrainedOverflowBox`…) | Debug: `FlutterError.onError`. Profile/release: `OverflowDetector` scan after settle, since the debug report sits inside `assert` (spike below) |
| `buildError` / `paintError` / `layoutError` | `FlutterError.onError` (`widgets library` / `rendering library`), including errors that render `ErrorWidget` | All |
| `imageLoadError` | `FlutterError.onError`, `library: image resource service` (404, decode failure) | All |
| `frameworkAssertion` | Other `FlutterError` assertions (`setState() called after dispose`, `Looking up a deactivated widget's ancestor`…) | Debug only |
| `uncaughtError` | `PlatformDispatcher.instance.onError` (uncaught async/microtask errors) | All |
| `appCrash` | CLI: driver connection lost and app PID gone. Excerpt from `logcat` (`AndroidRuntime` / `FATAL EXCEPTION` / `libc` tombstone for the PID) on Android, from the newest `~/Library/Logs/DiagnosticReports/<app>*.ips` on the iOS Simulator | All |

**Hook chaining.** Apps routinely replace `FlutterError.onError` after the binding is initialized (Crashlytics, Sentry). The driver therefore wraps whatever handler is current when the binding installs, re-checks the handlers at every RPC boundary and after the first frame, and re-wraps a handler that was replaced, always chaining to it. `PlatformDispatcher.onError` keeps its return contract: the wrapper returns the previous handler's result, or `false` when there was none, so default reporting is unchanged. Errors caught by an app's own `runZonedGuarded` handler are invisible to Tapwing. This is a documented limit.

#### Data model and step attribution

**Done (2026-11-21).** Driver: [`error_hooks.dart`](../../packages/tapwing_driver/lib/src/diagnostics/error_hooks.dart) and [`app_issues.dart`](../../packages/tapwing_driver/lib/src/diagnostics/app_issues.dart). CLI: [`app_health.rs`](../../crates/tapwing/src/app_health.rs) (policy, ignore rules, summaries, crash excerpts), `Driver::take_issue_batch` (step split by request id), and the orchestrator (attribution, `fail`, crash naming). Usage: [App health](../../docs/app-health.md).

- The driver keeps a bounded ring buffer (500 entries, with a `dropped` counter) of issue records: `id` (stable), `seq` (bumped on repeats), `kind`, `message` (first line), `library`, `summary` (trimmed `FlutterErrorDetails` text; overflows add bounds and the first text inside), up to 8 stack frames (app frames first), `count`, `firstSeenMs`/`lastSeenMs`, and `request`.
- **Attribution without extra round trips:** each issue carries the id of the RPC request that was in flight when it fired, or of the last completed request when it fired between requests. Issues caused by an action but surfacing after its response still belong to that action. The CLI maps request ids to flow steps. Issues before the first request belong to `launchApp` or attach.
- **Deduplication** by `kind` + message + first app frame per request. A repeat bumps `count` and `seq` and is re-sent with the same `id`; the CLI keeps the latest copy. Each overflowing render object is reported once, as in debug mode.
- **Delivery piggybacks on responses:** once the CLI sends `configure` `appIssues: true`, every success or error response carries `issues: [...]` (new or updated since the previous response). Older CLIs never see the field. The `appIssues` RPC (`since: <seq>`) re-reads the buffer at flow end or after a reconnect. Native-lane steps send no driver RPC, so the CLI calls `appIssues` after them.
- **Hooks are installed at binding init**, so startup errors are buffered and delivered with the `configure` response. `configure` `appIssues: false` (`--app-errors off`) uninstalls the hooks (restoring the previous handlers when Tapwing's are still outermost) and clears the buffer. The overflow scan runs after each settle in profile/release builds, and only while delivery is enabled.
- This step-event model is the **foundation for F8's trace viewer**. Network log, settle reasons, and screenshots later attach to the same `stepId`.

#### CLI and output

| Flag / config | Behavior |
|---|---|
| `--app-errors report` (default) | Issues are printed under the step (`⚠ layoutOverflow: RenderFlex overflowed by 12 px on the right — during tapOn "Pay"`), summarized per flow, and stored in `run.json`. The exit code is unchanged, so default runs keep Maestro pass/fail parity |
| `--app-errors fail` | A flow fails at the end of the first step that produced a non-ignored issue (`appCrash` always fails, as in Maestro). The failure message names the issue and its step |
| `--app-errors off` | `configure` `appIssues: false`: hooks are uninstalled, the buffer is cleared, and no overflow scans run |
| `--app-errors-ignore <regex>` (repeatable) | Ignores known issues by message, for example a third-party package overflow |
| `--app-errors-screenshot` | On the first occurrence of each fingerprint, takes a screenshot to `<test-output-dir>/app-issues/` |
| `config.yaml` → `tapwing: { appErrors: fail, appErrorsIgnore: [...] }` | Workspace default. CLI flags win. The installed Maestro 2.12.1 planner must accept the extra top-level key, checked in `cargo test --test workspace`. If it does not, the settings move to a separate `tapwing.yaml` |

`run.json`: each step gains `appIssues: [{id, kind, message, library, summary, stack, count,
request, firstSeenMs, lastSeenMs, ignored?, screenshot?}]` and each flow gains `appIssueSummary:
{total, ignored, byKind}`. JUnit (`--format junit`) adds `<system-err>` listing each step's issues,
and under `fail` the issue is the `<failure>`. HTML adds a `⚠ N app issues` pill in the flow header
(card kept open) and per-step lines.

**Known limits:** an issue that fires after the flow's last command is lost (the next flow's `launchApp` usually restarts the app). A crash is noticed when the socket closes or, if `adb reverse` keeps it open, by the liveness probe within ~1–2 s; a hung but live app fails after the 10 s silence timeout. Errors caught by the app's own `runZonedGuarded` handler are invisible. `--obfuscate` builds report mangled render object type names.

#### Spike: overflow in profile builds — **done 2026-11-21, detector ships**

The Android Quick Start builds **profile** APKs, where Flutter compiles out overflow reporting.
Pre-release evaluation chose option (a), a read-only detector, over (b), documenting a debug-build
requirement:
- [`OverflowDetector`](../../packages/tapwing_driver/lib/src/diagnostics/overflow_detector.dart) recomputes `RenderFlex._overflow` bit-identically from public geometry (`spacing`, children sizes in `_computeSizes` order) plus `RenderConstraintsTransformBox` overflow. It walks only painted, visible render objects and reproduces Flutter's message text exactly.
- **Debug parity:** 23 differential widget tests check the detector against Flutter's own `FlutterError` reports (same render objects, same messages). **Profile on device:** 9/9 expected cases found with 0 false positives, and 30/30 agreement with Flutter's internal `OVERFLOWING` flag.
- **Cost:** on `emulator-5554`, p50 25–83 µs and p95 ≤ 243 µs per scan across gallery, busy-list, and real sample app screens (3 passing flows). On physical **Pixel 8a** (`4C101JEKB02816`, Android 16 / API 36, 2026-11-22): `SPIKE verify` **9/9** (0 missing, 0 extra), oracle **31/31** agree (13 overflowing), gallery (202 render objects, 300 scans) p50 **48 µs** / p95 **433 µs** (max 5.6 ms), busy list (850 render objects, 300 scans) p50 **56 µs** / p95 **306 µs** (max 2.5 ms), and real app (39 scans across `login_and_browse`, `f2_selectors_and_taps`, `f2_swipe_and_scroll`, ≤ 445 render objects) p50 **463 µs** / p95 **921 µs** / max **943 µs**, **0 false positives** — well below the `< 2 ms` bar.
- **Design consequences:** scan once per step after settle in non-debug builds only (debug builds use `FlutterError.onError`); report only what is visible (scrolled-out content is reported once it scrolls into view); format geometry explicitly, because `toString` is stripped in profile.

#### Acceptance criteria

| Criterion | Status |
|---|---|
| Sample app `App Health` screen (Playground → App Health): overflow, broken image (offline port 9), build error, uncaught async error, `setState` after dispose, Java crash, `exit(3)`, unique/repeated error floods; flows `f7_app_health.yaml`, `f7_error_flood.yaml`, `ios/f7_app_health.yaml`, `negative/f7_app_crash.yaml` | **Met** |
| Flows pass in Maestro 2.12.1 and in Tapwing `report`; `fail` fails at the expected step with the expected kinds | **Met** on `emulator-5554` (profile: 4 kinds incl. detector overflow), `iPhone 18 Pro` (debug: 5 kinds incl. `frameworkAssertion`), and physical **Pixel 8a** (Android 16 / API 36, profile: `report` PASS 2 721 ms with 4 kinds / 5 issues, `fail` stops at `tapOn "Show overflow"`, `off` PASS with 0 issues, Maestro 2.12.1 14/14 PASS; `negative/f7_app_crash.yaml` detects Java crash in **95.3 ms** with 15-line `AndroidRuntime` excerpt) |
| Chaining: an app's own handlers installed later still get every error once, and Tapwing records once | **Met** (`app_issues_test.dart`) |
| Zero overhead without issues; `off` leaves no Tapwing hook | **Met:** `login_and_browse` median 7 122 ms (`report`) vs 7 070 ms (`off`) on `emulator-5554`, and **7 307 ms** vs **7 265 ms** (+42 ms / +0.58%) on Pixel 8a, within noise; the uninstall test restores the original handlers |
| Flooding stays bounded | **Met (2026-11-22):** O(1) `LinkedHashMap` ring buffer (500 entries) + wire `droppedIssues` surfaced in console and `appIssueSummary.dropped`. Verified on physical **Pixel 8a** (`f7_error_flood.yaml`, profile, 3 935 ms): 2 000 unique errors at **2 695 errors/s** → 120 delivered + 500 capped + **1 380 dropped** (2 000 exact); 2 000 identical errors at **2 695 errors/s** → 2 deduplicated entries (`×120`, `×1880`, 0 extra drops); post-flood `tapOn "Show overflow"` in 50.7 ms |
| Dart + Rust tests: classification, dedup, chaining, re-wrapping, attribution, policy, ignore, report shape, `optional`, crash, dropped flood counts | **Met:** 104 Dart tests (`packages/tapwing_driver`) + F7 Rust unit & integration tests (`f7_app_health.rs`) |

---

### 8.2 Real-project pilot

**Goal.** Run an existing production Flutter app and its existing Maestro suite on Tapwing, locally, without changing the app's production code, and explain every divergence from Maestro.

**Setup.** A `git clone --local` of the project (its working tree stays untouched) with `tapwing_driver` as a path `dev_dependency` and `lib/main_tapwing.dart` (`TapwingBinding.ensureInitialized(); app.main();`). The app has one `main.dart` and resolves its flavor at runtime, so `flutter build apk --profile --flavor dev -t lib/main_tapwing.dart` is enough.

| Phase | What | Result |
|---|---|---|
| **L0** Parse | `tapwing check .maestro/tests` | 35 valid, 1 invalid (an empty file); Maestro agrees. 4 ms |
| **L1** Toolchain | Driver on the app's Flutter 3.41.6 / Dart 3.11 | 95/95 Dart tests pass; the `>=3.47` constraint was too strict, now `>=3.41` |
| **L2** Integration | Instrumented dev profile build, first flow | Handshake on first launch (after `WidgetsFlutterBinding.ensureInitialized` and `Firebase.initializeApp` in the app's `main`) |
| **L3** Differential, pre-login | Two content flows and Login vs Maestro 2.12.1 | Identical results; Tapwing 3/3 stable. 27 s → 8.5–9.7 s, 48 s → 10.0–10.4 s |
| **L4** Auth0 login (hybrid) | Login through the Chrome Custom Tab, then post-login screens | Native lane types into Chrome and returns to the Flutter lane; found the `back` divergence below |
| **L5** App health | F7 during every run | 0 issues. Survival of the hooks after Crashlytics replaces `FlutterError.onError` is by design (re-wrap at every RPC), not proven live |

**Tapwing bugs found and fixed.**
- **`back` hid a real app bug.** The app sets `android:enableOnBackInvokedCallback="true"`. On one screen the framework does not claim Back, so the system Back backgrounds the app. Maestro (real `KEYCODE_BACK`) fails there; Tapwing delivered `popRoute` in-app and passed. Now Android `back`, `pressKey: Back` and `hideKeyboard` without a keyboard send a real `KEYCODE_BACK`. The driver snapshots state first (`armBack`), counts `popRoute` / `commitBackGesture` deliveries in `ObservingBinaryMessenger` (before the navigator, since `WidgetsBindingObserver`s run in registration order), and `awaitBack` reports `navigation`, `keyboard`, `background` or `none`. After `background` the next step checks window focus (as after `Home`) and the console warns. Cost unchanged (~70–85 ms per `back`).
- **`tapwing hierarchy` read the wrong app.** An instrumented app on a booted iOS simulator keeps dialing its last session port on the host loopback and could answer an Android session. The driver now rejects a `hello` whose `platform` differs; `hierarchy --app-id` also pins the PID on Android.

**Environment notes (not Tapwing bugs).** The flows assert Czech text, and `clearState` (`pm clear`) also clears the per-app locale, so the emulator's system locale must be `cs-CZ`. Chrome keeps the Auth0 SSO session across `clearState`, so a second login lands on the consent screen instead of the form. The project's `Login.yaml` passes vacuously in both tools: Auth0 no longer exposes `id: username`, so every conditional step is skipped.

**Since then.** `tapwing init`, the release tarball (§8.3) and the iOS Simulator half of the pilot are done (the iOS fixes landed as PR #5); what is still open is listed under the follow-ups after the F11b results.

### 8.3 F11: Own native drivers

**Why.** The native lane was Tapwing's only runtime dependency on Maestro. On Android it meant two APKs (`maestro-app.apk` with the target package and a Unicode IME, `maestro-server.apk` with the UiAutomator instrumentation and gRPC server) and switching the device's default IME for every non-ASCII `inputText`, a known source of flakiness. On the iOS Simulator it meant bundling Maestro's ~20 MB `maestro-ios-driver.jar` (built with FlyingFox + dumped private headers). F11a replaces the Android APKs and F11b replaces the iOS Simulator XCUITest runner behind the shared `--native-driver tapwing|maestro` selector.

**F11a design.**
- **One APK, same wire.** `drivers/android` builds `dev.tapwing.driver`: a plain `Instrumentation` that targets its own package and serves `maestro_android.proto` (the file in `crates/tapwing/proto`, shared) over grpc-okhttp on device port 7001. The Rust client is unchanged. The default switched to `tapwing` on 2026-11-22 once F11b landed, and **Maestro's native drivers were removed on 2026-11-22** after physical Pixel 8a (Android 16 / API 36) parity verification: deleted `maestro-drivers.sha256`, the `NativeDriver` enum, Maestro APK/jar extraction, `ImeGuard` / Unicode-IME switching, the iOS jar extraction path, `TAPWING_MAESTRO_*`, and Maven Central downloads in `scripts/dist.sh` (`maestro_drivers.rs` → `driver_bundle.rs`, `kind.rs` → `command.rs`, `native_hierarchy_diff` kept as a Tapwing-only diagnostic). `--native-driver` and `config.yaml` `tapwing.nativeDriver` remain as deprecated no-ops (`tapwing` accepted silently; `maestro` or any other value prints a deprecation warning and uses Tapwing's driver). The Apache-2.0 attribution for the ported code (`drivers/*/THIRD_PARTY_NOTICES.md`, file headers, root `LICENSE`) and `maestro_android.proto` stay unchanged.
- **Maestro's semantics by porting, not re-implementing** (Apache-2.0, attributed per file, `drivers/android/THIRD_PARTY_NOTICES.md`): the hierarchy dumper with the same UiAutomator 2.3.0 calls (`getWindowRoots`, `waitForIdle(500)` + cache reset before each dump, attribute set and order, visible bounds, WebView children, toast node), `clickNoSync` taps, ASCII `inputText` as one key press per character 75 ms apart, `KEYCODE_DEL` erase without animation waits, screenshots, `waitForWindowUpdate`, mock location providers. Tapwing calls 9 RPCs (`deviceInfo`, `viewHierarchy`, `tap`, `inputText`, `eraseAllText`, `screenshot`, `isWindowUpdating`, `setLocation`, `enableMockLocationProviders`); `disableLocationUpdates` is served too, `launchApp` / `addMedia` answer `UNIMPLEMENTED`.
- **Unicode without an IME switch.** Maestro commits non-ASCII text through its IME (`commitText(text, 1)`: inserted at the cursor, replacing a selection, cursor after it). The driver reproduces that result with accessibility: it finds the input-focused editable node (falling back to a bounded search when `findFocus` returns a host view, as Chrome does), computes the spliced text from its text and selection, applies `ACTION_SET_TEXT`, then `ACTION_SET_SELECTION` when the cursor is not at the end. Only password fields (masked text) and editors without `ACTION_SET_TEXT` fall back to an IME inside the same APK, which runs in the driver's own process, so no broadcasts are needed; the previous IME is restored before the RPC returns. Clipboard + paste was rejected: it overwrites the user's clipboard and Android 13+ shows a clipboard overlay that lands in hierarchies and screenshots.
- **Signing and build.** A committed, intentionally public test key keeps the signature stable so any build updates any older driver in place (`drivers/android/signing/README.md` explains why it is not a secret). `scripts/build-android-driver.sh` builds a reproducible APK (Play's encrypted dependency block disabled) plus its SHA-256; `scripts/dist.sh` bundles it as `share/tapwing/drivers/android/tapwing-driver.apk` with a `.sha256` sidecar that the CLI checks. Lookup: `$TAPWING_DRIVER_APK`, the bundle, then the checkout build.

**Results — Emulator** ([bench/results/f11a/summary.md](../../bench/results/f11a/summary.md), `emulator-5620`, API 35).

| Criterion | Result |
|---|---|
| Hierarchy parity, ≥ 8 screens | **Met:** 11 screens (launcher, Settings ×2, Chrome page, Chrome web input, notification shade, quick settings, permission dialog, 3 sample-app screens incl. soft keyboard): 10 byte-identical, 1 identical up to window order (UiAutomator's `HashSet` of window roots; Maestro's own dumps vary the same way). No accepted XML difference |
| Differential flows, `auto` and `native` | **Met:** all 11 Android flows give the same result with both drivers in both modes (auto 9/11, native 7/11; the shared failures are environment or pre-existing native-lane gaps, also with Maestro's driver). Fresh install from the release bundle passes |
| Default IME unchanged | **Met** after every run |
| Timings | Start 142 ms vs 521 ms (warm), 0.42 s vs 0.66 s (fresh install); `viewHierarchy` p50 10 ms vs 59 ms on identical XML; Unicode `inputText` 4–11 ms vs ~450 ms; APK 4.0 MB vs 12.6 MB |

**Results — Physical Pixel 8a** ([bench/results/f11a/pixel-8a.md](../../bench/results/f11a/pixel-8a.md), `4C101JEKB02816`, Android 16 / API 36, Gboard, 2026-11-22).

| Criterion | Result |
|---|---|
| Hierarchy parity, 11 screens | **Met:** 0 differences — 9 byte-identical, 2 (`settings`, `sample_text_keyboard`) identical up to window order; `viewHierarchy` p50 **1.4–2.6× faster** (launcher 241 → 163 ms, Settings 424 → 309 ms, keyboard 560 → 367 ms), warm start ~2× faster |
| Unicode & default IME | **Met:** `Příliš žluťoučký kůň` in **80 ms** vs **1 095 ms** (Maestro); Gboard unchanged after every run |
| Mock location (physical device) | **Met:** `gps`, `network`, and `fused` test providers updated in **243 ms** vs **369 ms** (Maestro). No Developer options → "Select mock location app" required on API 36 (`android:mock_location` appop self-grant suffices while Developer options are enabled) |
| Differential flows (`auto` & `native`) | **Met (16/16):** `f5_permission_dialog`, `f11_native_unicode_input`, `login_and_browse`, `f2_text_clipboard_keys` × `auto`/`native` × both drivers (native times Tapwing vs Maestro: 7.2 vs 9.4 s, 22.6 vs 27.6 s, 40.3 vs 47.5 s, 60.9 vs 64.7 s); cold install 2 227 ms vs 2 438 ms |
| Post-removal verification | **Met:** after uninstalling `dev.tapwing.driver` and all `dev.mobile.maestro*` packages, fresh-install runs pass in `auto` (`f5_permission_dialog` 4.2 s, `f11_native_unicode_input` 3.6 s, `login_and_browse` 7.5 s) and `native` (`f5_permission_dialog` 6.1 s, `f11_native_unicode_input` 21.3 s), `--native-driver maestro` emits deprecation warning and runs Tapwing's driver, and the unpacked **9.3 MB** release tarball passes `f5_permission_dialog` in `native` with zero `dev.mobile.*` packages |

**Found on the way.** (1) Tapwing with Maestro's driver kept Maestro's IME selected for the whole session, and later ASCII key presses into Flutter fields were lost; Maestro restores the IME after each Unicode input, and now Tapwing does too (prior to Maestro driver removal). (2) Maestro 2.12.1 cannot type non-ASCII text into a web `<input>` in Chrome (its IME commit never lands); Tapwing's driver can. This is the one accepted behavioural difference. (3) On this host, SwiftShader emulators drop their adb transport when a soft keyboard appears; with `-gpu host` they do not, and after a drop the native lane does not reconnect. (4) On Pixel 8a, Chrome showed a work-profile "Sign in to managed account" dialog, so Chrome web-input parity and `chrome_web_unicode.yaml` were not exercised on that phone; `f4_device_commands` was skipped on the personal device (`setLocation` tested separately).

**Not verified yet:** OEM IMEs other than Gboard, Android API levels other than 35/36, Compose apps, real-project flows.

**F11b design (iOS Simulator).**
- **Why not AXe:** the half-day AXe v2.1.1 spike (`spike/f11-ios-axe`) returned **NO-GO** — AXe misses SpringBoard permission alerts (invisible to `describe-ui`), mangles Czech Unicode diacritics via HID keycodes, lacks orientation/elementType/value/placeholder parity, and depends on private macOS CoreSimulator/FBSimulatorControl frameworks.
- **Own XCUITest runner, zero external deps.** `drivers/ios` builds `TapwingRunner.app` (`dev.tapwing.ios-runner`) + `TapwingRunnerUITests-Runner.app` (`dev.tapwing.ios-runnerUITests.xctrunner`) from a committed, minimal `TapwingRunner.xcodeproj` with zero SPM/CocoaPods dependencies (Apple SDK only: `XCTest`, `Network`, `UIKit`, `CryptoKit`, `Foundation`). A ~150-line `Network.framework` (`NWListener`, `TCP_NODELAY`) keep-alive HTTP/1.1 server serves all 14 routes consumed by `crates/tapwing/src/ios_native_lane/client.rs` unchanged.
- **Parity & ObjC runtime bridge.** One small ARC-managed `XCTestRuntimeBridge.{h,m}` replaces Maestro's dumped PrivateHeaders tree: it suppresses automatic UI interruption dismissal (`doesNotHandleUIInterruptions`), enumerates active apps (`TapwingActiveAppsInfo`), unifies `XCAXClient_iOS.defaultParameters` swizzling (`maxDepth` + `snapshotKeyHonorModalViews`), and dispatches touch/swipe/text synthesis without raw Swift `unsafeBitCast`. Pure logic (`HttpFraming`, `Models`, `OrientationMath`, `classifySwipeError`) lives in `drivers/ios/Core/` and runs host-side unit tests via `swift test` in ~2 ms with no booted simulator.
- **Build, lookup & packaging.** `scripts/build-ios-driver.sh` runs `swift test`, builds a universal `arm64 + x86_64` simulator bundle (375 KB zipped vs Maestro's ~7.3 MB / ~20 MB jar), normalizes `tapwing-ios-runner.xctestrun` (`TestTimeoutsEnabled = false`, attachments `keepNever`), and writes `.sha256` sidecars. `ios_native_lane::tapwing_driver` resolves `$TAPWING_IOS_DRIVER_DIR` → bundled `share/tapwing/drivers/ios/` (sidecar-verified, atomically unpacked into `~/.tapwing/ios-native-driver/tapwing-<hash>/`) → checkout `drivers/ios/build/`. `scripts/dist.sh` bundles it in `*-apple-darwin` tarballs and self-checks `tapwing --version`. `IosNativeSlot::Lazy` is preserved (0 native calls / 0 ms overhead on pure-Flutter flows).

**F11b results** ([bench/results/f11b/summary.md](../../bench/results/f11b/summary.md), `Tapwing-AXe`, iPhone 18 Pro, iOS 27.2, Xcode 27.2).

| Criterion | Result |
|---|---|
| 14-endpoint wire & keep-alive parity | **Met:** all 14 routes + 404/400/408 error semantics verified over a single persistent HTTP/1.1 connection; Czech Unicode (`Příliš žluťoučký kůň`) + `eraseText` + rotations + `pressButton(home)` pass |
| Hierarchy parity vs Maestro 2.12.1 | **Met:** 0 differences on identical login screen (`only_tapwing=0, only_maestro=0`) |
| Differential flows (`auto` & `native`) | **Met:** `ios/f5_permission_dialog.yaml` passes in `auto` (7.1–9.3 s) & `native` (8.6 s) matching Maestro (7.6 s); `ios/login_and_browse.yaml` stays 100% pure Flutter in `auto` (`flutter: 27, native: 0`) and passes in `native`; `f2_swipe_and_scroll`, `f2_text_clipboard_keys`, `f2_hide_keyboard`, `f2_ios_device_commands` all pass in `native`; fresh bundled tarball from `scripts/dist.sh` passes `f5_permission_dialog.yaml` |
| Timings & bundle size | Cold start **2 480 ms** vs 2 866 ms; `viewHierarchy` p50 **41.9 ms** vs 43.0 ms; `touch` **349.9 ms** vs 349.3 ms; `screenshot` **48.7 ms** vs 48.0 ms; bundled archives **375 KB** vs **7.3 MB** (~19.5× smaller) |

#### CI & multi-target distribution

- **CI ([.github/workflows/ci.yml](../../.github/workflows/ci.yml)):** runs every test suite on `pull_request` (with concurrency cancel) and `push` to `main` on `ubuntu-latest`, with no emulator or simulator, so it stays deterministic. Jobs: `Rust` (`protoc` installed, required by `crates/tapwing/build.rs`; `cargo fmt --check`, `cargo clippy --locked --all-targets -- -D warnings`, `cargo test --locked -p tapwing` including the `cli_e2e` tests of the compiled binary, `cargo check --locked` of `bench/parse`, and `tapwing check` over the sample app and bench flows), `Dart & Sample App` (`flutter analyze` + `flutter test` in `packages/tapwing_driver` and `examples/sample_app`, on Flutter `3.47.5` with strict analysis and on `3.50.0` where only infos are non-fatal because `package:flutter/material.dart` is deprecated there), `Android driver` (`scripts/build-android-driver.sh`: Kotlin unit tests + release APK), `Release scripts` (`scripts/test-dist.sh`), and `CI Gate`, the single check the `main` ruleset requires: it fails when any other job failed, was cancelled or was skipped. (The iOS Simulator driver is built and tested on `macos-latest` in `release.yml`.)
- **Release workflow ([.github/workflows/release.yml](../../.github/workflows/release.yml)):** runs on `v*` tags and `workflow_dispatch`, building `aarch64-apple-darwin`, `x86_64-apple-darwin` (both on `macos-latest`, bundling Android + iOS drivers), and `x86_64-unknown-linux-gnu` (on `ubuntu-22.04`, so the binary needs glibc 2.35 or newer; bundling Android driver only) via `scripts/dist.sh` and publishing `.tar.gz` + `.sha256` assets. A `workflow_dispatch` run with a `tag` is checked against `Cargo.toml` and tags the commit it ran on; `install.sh --version` refuses to install a release that has no `.sha256`. macOS codesign and `notarytool` notarization steps are gated on `APPLE_*` secrets (documented in [docs/install.md → Releasing](../../docs/install.md#releasing)); notarization fails the job unless notarytool's JSON status is `Accepted`. Third-party actions are pinned by major tag, not SHA: pin `softprops/action-gh-release`, `Swatinem/rust-cache` and `gradle/actions/setup-gradle` to SHAs before the first signed release.
- **Status:** `x86_64-apple-darwin` release compilation, `scripts/dist.sh` / `scripts/install.sh` multi-target packaging & `uname` detection (`scripts/test-dist.sh`), and `packages/tapwing_driver` `flutter analyze` + `flutter test` are verified locally. Native `ubuntu-latest` compilation/tests (blocked locally by missing Linux C cross-compiler for `ring` / `rquickjs-sys`), live GitHub Actions workflow execution, and secret-gated macOS codesign/notarization remain to be verified on the first CI/release run.

**Follow-ups (native lane, both drivers):**
- **Reconnect after an adb transport drop — Android done 2026-11-22; iOS runner done 2026-11-22.** When the adb connection drops mid-run (adb server restart, USB link, the on-device process killed), the `adb forward` and the instrumentation die and every later native RPC used to fail at once (`tcp connect error`). `NativeDriverClient` now owns its session's teardown guards and a restart function. On a *transport* failure (tonic `Unavailable` / `tcp connect error`, or a `transport error` from a connection that broke) it tears the dead session down first (its `force-stop` would otherwise kill the new instrumentation), starts a new one (install check, forward, `am instrument`, readiness probe; retried for up to 30 s while the device is `offline`, `TAPWING_NATIVE_RESTART_TIMEOUT_MS`), and repeats the RPC. Reads and idempotent writes are always repeated. `tap`, `inputText` and `eraseAllText` are repeated only when the connection was refused, so the request provably never left the host; if the connection broke under them the driver is restarted but the step fails with `not repeated because it may already have been applied`, never a double tap or doubled text. An answer from the driver, including an error status, and an RPC timeout never restart. At most 3 restarts per run, then the transport error is fatal. If the restart itself fails the original error is kept and later calls fail fast. A restart prints one `warning:` line (also when it happens while capturing failure diagnostics), starts the new session with the screen size uncached and no mock-location providers (a `setLocation` replayed after a restart enables them first, since the fresh driver would otherwise accept it and drop it, and a restart that happened elsewhere leaves the flag clear so the next `setLocation` enables them itself). A reply body that dies mid-stream (`h2 protocol error`) counts as a broken connection, and a deadline (`Cancelled: Timeout expired`, which tonic wraps in the same `transport::Error`) does not. Tests (`native_lane`): real tonic error classification (including a hanging server and a connection cut mid-reply), refused / broken / server-gone / failed-restart / cap / teardown-order cases against the fake gRPC driver, `retry_until`. Two independent review passes (the Neon agent, then Claude Opus 5.5) found the timeout and mid-reply misclassifications, the mock-provider gap and the missed notice; all four are fixed, with the reproducers kept as regression tests.
  - **iOS runner (same design over HTTP):** `IosNativeClient` owns the `xcodebuild` process and a restart function (the warm-up's starter and cancel flag). A refused connection, a reset, or a socket closed before or during the reply (`UnexpectedEof`) tears the dead runner down first (its `simctl terminate` would kill the new runner), starts a new one on a new port, waits for `/status` and repeats the call; reads and `setOrientation` always, `touch` / `swipeV2` / `inputText` / `eraseText` / `pressKey` / `pressButton` only when refused. An HTTP answer never restarts, nor does a timeout while `xcodebuild` is alive (after it exited, a timeout counts as broken). `/status` never restarts; same cap, failed-restart and `warning:` rules as Android, printed on every native path including failure diagnostics. `Lazy` slots are untouched. Tests: real-socket classification (closed port, accept-then-close, reset, cut mid-body, hang), per-route replay, teardown order, cap, failed restart, healthy run, cancel, notice draining. Live on the `iPhone 18 Pro` simulator: a 40 s polling step with the runner app or `xcodebuild` killed 5 s in fails before (`Peer disconnected` / `Connection refused`) and passes after with one `restart 1/3` warning, new port, no stray process or listener; `f5_permission_dialog` passes in `auto` and `native` without a warning, `login_and_browse` never starts `xcodebuild`. Review (Claude Opus wrote it, Neon reviewed it): `ureq` never re-sends a POST on a reused pooled connection (it probes the pooled socket first and only retries redirects), so the replay policy is the only replay; a connection refused is never reported for a request that was sent. One finding fixed: a focus probe on a dead runner asked a second question and burned two of the three restarts (or waited two 30 s timeouts); only the runner's own HTTP error answer (`RunnerAnswerError`) now justifies it. Re-verified live by the coordinator on the integrated binary (iPhone 18 Pro, `--input native`, 40 s polling step, process killed after 16 s): killing the runner app and killing `xcodebuild` both give one `restarted it (restart 1/3)` warning and a pass, the pre-change binary fails with `viewHierarchy request failed: io: Peer disconnected`, nothing is left behind, and `f5_permission_dialog` (`auto`, `native`) and `login_and_browse` (`auto`) pass with no warning.
  - Android: verified live on the physical Pixel 8a (Android 16, API 36, 2026-11-22): a native flow with a 40 s polling step and native steps after it, cut 14 s in. `adb kill-server` and `am force-stop dev.tapwing.driver` both made the pre-change binary fail with `native driver viewHierarchy failed: transport error`; with the restart the flow passes with one `restarted it (restart 1/3)` warning, the later `tapOn` / `inputText` / `back` steps run on the new driver, and nothing is left on the device (driver process, forward, temp log, `stay_on_while_plugged_in`). Also live: `adb usb` (adbd restarts, the device is `offline` for a few seconds: the restart waits for it and the flow passes); a `setLocation` loop with the driver killed mid-loop (restart lands on a `setLocation` RPC, `h2 protocol error` cause, replayed step 1.5 s including the whole restart, `dumpsys location` shows fresh gps / network / fused mock providers from the new process); and the four regression flows `f5_permission_dialog`, `f11_native_unicode_input`, `login_and_browse`, `f2_text_clipboard_keys` in `auto` and `native` (8/8 pass, same times as the F11 runs, no restart warning on a healthy run). Not exercised: physically pulling the USB cable.
- **Mock-location providers outlived the run on a physical device — resolved 2026-11-22.** Found while verifying the reconnect on the Pixel 8a, not caused by it. The driver serves `disableLocationUpdates` (it disables and removes the gps / network / fused test providers) but the CLI never called it, so after any flow that used `setLocation` or `travel` the phone kept reporting the last mocked position (`dumpsys location` listed `gps provider [mock]` etc.) until the providers were removed or the device rebooted. The providers belong to the OS, not to the driver process, so stopping the instrumentation does not remove them, and `adb shell cmd location providers remove-test-provider` does nothing for the shell uid unless `appops set --uid 2000 MOCK_LOCATION allow` is set first. `NativeDriverClient` now calls `disableLocationUpdates` in its `Drop`, when it had enabled them, before the guards stop the instrumentation: one attempt capped at 3 s, no restart, errors ignored, and nothing at all for a session that never mocked a location. A session replaced by a restart is not contacted (it is dead); the replacement cleans up the providers it re-enabled for a replayed `setLocation`. Verified live on the Pixel 8a with a 40-step `setLocation` flow: the pre-fix binary leaves 3 mock providers behind, the fixed one leaves 0 (and also clears those left by the pre-fix run), with the same run time (2.6 s), and after a run in which the driver is killed mid-loop and restarted.
- **Graceful `SIGINT` / `SIGTERM` device teardown — resolved 2026-11-23.** Previously, pressing `Ctrl-C` (`SIGINT`) or cancelling a CI job (`SIGTERM`) terminated the CLI immediately without unwinding, leaving `adb reverse`, `adb forward`, `stay_on_while_plugged_in`, `disableAnimations` scales (`0`), OS mock-location providers, `dev.tapwing.driver` / `xcodebuild`, or temp screen recordings on the device. [`interrupt.rs`](../../crates/tapwing/src/interrupt.rs) now installs a `SA_RESTART` signal handler: the first `SIGINT` or `SIGTERM` sets an atomic interrupt flag, prints an async-signal-safe notice to `stderr`, aborts wait/poll loops with `InterruptedError` (never retried by `retry`, never downgraded by `optional`, skipping `onFlowComplete`, crash diagnosis, failure capture, and partial report writes), unwinds the stack so every RAII guard cleans up the device, and exits with standard Unix status `128 + sig` (`130` for `SIGINT`, `143` for `SIGTERM`). A second signal calls `_exit(128 + sig)` immediately so a hung ADB daemon never traps the user. Verified by unit tests (`interrupt::tests`), mock-ADB E2E tests (`cli_e2e::signal_cleanup`), and live on `emulator-5554` (`--input native` + `disableAnimations: true` interrupted with `SIGINT`: exit `130`, `adb reverse` and `adb forward` empty, `dev.tapwing.driver` stopped, `window_animation_scale` / `transition_animation_scale` / `animator_duration_scale` restored).
- **Two native-only flow failures — resolved 2026-11-22** (Pixel_8_Pro_API_35 emulator, 1344×2992, `-gpu host`). Neither was in the driver; both were native-lane (Rust) gaps against Maestro's semantics:
  - `assertVisible: {text: "Traits Ready", traits: text}` failed because Maestro's `Filters.hasText` (decompiled from 2.12.1) is `attributes["text"] != null`: it tests that the attribute *exists*, and every UiAutomator node carries `text` (empty for a Flutter label that lives only in `content-desc`). The native lane collapsed empty attributes to `None` and checked non-empty. `TreeNode` now has `has_text_attr`, set by the Android hierarchy parser; `LONG_TEXT` still uses the text length, other lanes unchanged.
  - Found while repeating the flow: `tapOn: {text: "Covered target", waitUntilVisible: true}` tapped the 900 ms "Temporary cover" overlay (the native lane ignored `waitUntilVisible`), failing `Covered tapped` in 3 of 5 runs. The lane now ports `Maestro.tap`: when the first tap changed nothing and the target is not the topmost element at its centre (`ViewHierarchy.isVisible`, `getElementAt`), it polls up to 10 s until the target is uncovered and taps again. Follow-up (2026-11-23, found in 1 of 20 loaded native stress runs): when the overlay covered the target at the moment of the first tap (`Hierarchy::is_covered_by_other`) and vanished *during* post-tap `settle()`, `outcome.changed` became `true` and skipped the re-tap; `tap_on_element` now also re-taps when an external overlay covered the target before the tap and the target's own subtree signature (`TreeNode::structure_signature`, which now includes `accessibility_text` / `content-desc` and `hint_text`) is unchanged after `settle()`.
  - `scrollUntilVisible "Snap Page 3"` (horizontal `PageView`) no longer reproduces: 5/5 passes in Tapwing native (3 before and 2 after the fixes above) and 3/3 in Maestro 2.12.1 native on this emulator. It failed 3/3 on the F11a emulator, which no longer exists, so the cause was not isolated; treat it as environment-dependent until it reappears. Native swipes and `scrollUntilVisible` were reviewed against `AndroidDriver`/`Orchestra` and match.
  - Native `f2_selectors_and_taps` before: Tapwing 1/3, Maestro 1/3 (Maestro also flaked on this loaded host, at other steps); after the two fixes Tapwing 6/6. Regression after the change on a freshly booted emulator: `f5_permission_dialog` native 2/2, `f2_text_clipboard_keys` native 1/1, `f2_selectors_and_taps` auto 1/1, 258 unit tests green (new: `text` trait presence, `isVisible`/`getElementAt`, `waitUntilVisible` retap and no-retap).
- **Cold-start flakiness on a loaded 3 GB emulator — resolved 2026-11-23 (bounded retry in code 2026-11-22, native lane checked live on the Pixel 8a 2026-11-23, 50-run stress suite + live Flutter-lane silence retry verified on `emulator-5554` 2026-11-23).** About 1 run in 5 failed before its first real step, in both Tapwing lanes (Maestro showed the same symptom once, `Id matching regex: nav_selectors_taps` not found): `tapOn id: playground_button` not found for 20 s because the freshly cleared Flutter app was not yet in the UiAutomator tree (native), or `configure`/`waitIdle` got no answer for 10 s (Flutter lane), with several emulators and builds running on the host (see also the F8b results). The root cause is **unproven** (a starved app isolate and a stalled `adb reverse` tunnel both fit the Flutter-lane data; the native symptom fits a cold start longer than the 17 s lookup). Maestro 2.12.1 has no equivalent (decompiled: `Orchestra.launchAppCommand` → `Maestro.launchApp` → `AndroidDriver.launchApp` is `open()` + `isPackageInstalled` + one gRPC `launchApp`, no retry, no settle). Fix, one bounded retry per launch, nothing extra on a healthy run (no RPC, no wait): **Flutter lane:** when the first `configure` or launch `waitIdle` fails with exactly the 10 s silence (not a closed socket, a dead process or a driver error), the connection is dropped, the same pid gets 10 s to dial in again, and both calls (idempotent) are sent once more; the whole retry (reconnect plus handshake) shares a budget of two silence windows (20 s), every call's silence window is cut to what is left of it, and a retry that is silent again fails with the first failure's error. The app's issue and request-log sequence numbers continue across the reconnect, so the host keeps its watermarks (nothing already delivered is fetched or counted twice). `--maestro-compat=strict` does not disable this lane's retry: it is a reconnect of Tapwing's own in-app channel, which Maestro has no equivalent of, and changes no Maestro-visible lookup. **Native lane** (`--input native` and black-box apps, Android): if the app shows no content (no node of its package with text, description or id) when the launch settles, the first required lookup (not `optional`, > 7 s, not negative; an optional `assertVisible` / `extendedWaitUntil` never uses it up) that times out without ever seeing the app gets one more 10 s window; the arming is dropped by `stopApp`, `killApp`, `clearState`, `openLink` and `pressKey: Home`, and "shows content" counts text, description, id, hint text and error text; a lookup that saw the app fails exactly as before, and `--maestro-compat=strict` never arms it. Each retry prints one `warning:` line; the JSON report gets `coldStartRetries` (only when > 0, and also when the retry itself failed) and the launch step `phases.handshakeRetries` (`steps[i].driver.phases.handshakeRetries`; the trace carries the step field but not the flow total). Not covered: the 15 s `auto` hello window (a slower hello still falls back to black-box), iOS native lookups. Tests: `run::flutter_ops::tests` (6), `native_lane::ops` cold-start tests (4), hierarchy, arming and silence-error tests. Live on the physical Pixel 8a (Android 16, 2026-11-23, native lane): a cold start frozen on purpose (`run-as dev.tapwing.sample_app kill -STOP <pid>` on the process that appears after `launchApp`, `kill -CONT` 26 s later; flow `launchApp: clearState` + `assertVisible: "Welcome back"`, `--input native`). The pre-change binary fails the assertion after 17.0 s (the UI tree holds only systemui nodes); the new binary prints the `showed nothing on screen during the first 17033 ms` warning, passes once the app resumes (`assertVisible` 26.9 s) and reports `coldStartRetries: 1`. When the freeze lands inside `launchApp` instead (11.6 s there) there is no warning and the run passes as before, i.e. the retry fires only when the lookup itself saw nothing. The eight regression runs (`f5_permission_dialog`, `f11_native_unicode_input`, `login_and_browse`, `f2_text_clipboard_keys` × `auto` / `native`) pass with 0 warnings and no `coldStartRetries`; nothing is left on the device afterwards (no forward, no driver process, no stopped process, no mock providers). Live on a cold-booted 3 GB API 35 emulator (`tapwing-f11a` / `emulator-5554`, `-gpu host`, 2026-11-23): (a) **50-run stress suite** (6 guest CPU busy loops, 10× `3406a98` and 10× `HEAD` alternating on `f7_app_health.yaml --input flutter` and `f2_selectors_and_taps.yaml --input native`, then 5× unloaded per lane): Flutter loaded 10/10 old (median 3 245 ms, `launchApp` 1 465 ms) and 10/10 new (median 3 186 ms, `launchApp` 1 462 ms); Native loaded 10/10 old (median 43 123 ms, `launchApp` 1 105 ms) and 9/10 new (median 43 047 ms, `launchApp` 2 086 ms; run #1 failed later at step 22 `Covered tapped` when the 900 ms overlay disappeared during post-tap `settle`, not at cold start); unloaded 5/5 Flutter (median 2 439 ms, `launchApp` 906 ms) and 5/5 Native (median 42 075 ms, `launchApp` 1 474 ms) with 0 warnings and 0 `coldStartRetries`. (b) **Live Flutter-lane cold-start silence** (`su 0 kill -STOP <pid>` for 12 s right after `:7781` `ESTABLISHED` during `configure`): `3406a98` fails `launchApp` after 10 683 ms (`connection to app lost during configure: no response for 10 s`); `HEAD` prints `warning: launchApp dev.tapwing.sample_app: the app (pid 15993) went silent right after its cold start (...); reconnecting once (cold-start retry, at most 20 s more)`, reconnects the same PID on `kill -CONT`, passes all 14 steps of `f7_app_health.yaml` (`launchApp` 12 898 ms, total 14 455 ms), and records `coldStartRetries: 1` and `phases.handshakeRetries: 1`.


#### Cold-start retry: stress recipe (for the coordinator, on a device)

1. A cold-booted 3 GB API 35 emulator with the sample app (profile build) installed; no other emulator or build running at first. Build both binaries: the commit before the retry (`3406a98`) and the commit with it, each in its own worktree and target dir (a shared target dir can leave a stale binary; check with `strings` for `went silent right after its cold start`).
2. Load: `adb -s <serial> shell 'for i in 1 2 3 4 5 6; do (timeout 1200 sh -c "while :; do :; done" </dev/null >/dev/null 2>&1 &); done'` (6 busy loops as in F8b, ~400 % emulator CPU; they stop by themselves after 20 min). The redirections matter: without them the shell session stays open until the loops end and `adb shell` does not return. Before each batch of runs kill the old loops (`adb shell pkill -f 'while :; do :; done'`) so batches shorter than 20 min do not stack.
3. Runs: `examples/sample_app/.maestro/f7_app_health.yaml` with `--input flutter` and `f2_selectors_and_taps.yaml` with `--input native` (both start with `launchApp: clearState: true`), 10× each per binary, old and new alternating (40 runs), each with `--report <dir>/run-N.json`. Then 5 unloaded runs per lane with the new binary.
4. Record per run: pass/fail, the failing step and its message, total and `launchApp` time (`phases`), the `warning:` lines (`went silent right after its cold start`, `showed nothing on screen during the first`), `coldStartRetries` / `phases.handshakeRetries` from the report, and the process start delay (`adb logcat -b events -d | grep am_proc_start`).
5. Expect: the old binary fails before step 1 in about 1 of 5 loaded runs; the new one turns those into one warning and a pass, or fails at most 20 s (Flutter) / 10 s (native) later with the same message. The unloaded runs show no warning, no `coldStartRetries`, and the same times as the old binary.
---

### 8.4 F8b: Trace viewer

**Goal.** After any run, open one offline HTML file and walk the flow step by step: what ran, on which lane, how long it took, what the app reported (F7), what the screen looked like, and which HTTP requests the app made during the step. Playwright's trace viewer is the model; everything stays local and private.

**Contract fit (§2.2).** Observation only. No new YAML commands, so flows still run in Maestro unchanged. Without `--trace`, nothing changes: the CLI makes no extra calls, writes no extra files, and the driver keeps its HTTP request log disabled (only the existing in-flight counter used for idleness runs).

#### CLI and output

| Flag | Behavior |
|---|---|
| `--trace` | Writes `trace.json` + `trace.html` per shard into the shard's test output dir (`--test-output-dir` / `testOutputDir` / `.tapwing/run-*`; `shard-N/` when sharded). With `--flatten-debug-output` and several shards the files are named `trace-shard-N.*`, so parallel shards never collide. Each file covers every flow the shard ran |
| `--trace-screenshots failures` (default) | Screenshot and pruned UI tree only for failing steps, reusing the F8a failure capture (no extra device calls) |
| `--trace-screenshots steps` | Also captures a screenshot and pruned UI tree after every UI step (not for wrappers like `runFlow`/`repeat`/`retry`). Full PNGs go to `trace/` next to the trace; the HTML inlines ≤ 360 px JPEG thumbnails |

#### Trace model (`trace.json`, `schema: "tapwing.trace/1"`)

- **Run:** `schema`, `tapwingVersion`, `createdEpochMs`, `shard` (`index`, `device`, `platform`), `options` (`screenshots`), `flows[]`.
- **Flow:** `name`, `path`, `status`, `startEpochMs`, `totalMs`, `lane`, `error`, `appIssueSummary`, `http` (`enabled` / `unsupported` for drivers without the log / `off`), `httpDropped`, `steps[]`.
- **Step:** the F7 step fields (`index`, `command`, `description`, `status`, `ms`, `error`, `warning`, `appIssues`, `failure`) plus `startMs` (offset from flow start), `depth` (`runFlow`/`repeat`/`retry` nesting), `attempt` (1-based attempt of the innermost enclosing `retry`), `lane` (`flutter` / `native` / `hybrid` / `host`), `http[]`, and with `steps` screenshots `screenshot` + `tree`. Steps are listed in start order, so nested steps follow their wrapper.
- **HTTP entry:** `method`, `url`, `status`, `state` (`completed` / `failed` / `aborted` / `upgraded` / `incomplete` when the body was never drained / `inFlight` at flow end), `startMs` (driver clock), `durationMs`, `headersMs`, `requestBytes`, `responseBytes`, `error`, `requestHeaders`, `responseHeaders`, `request` (RPC id).

#### `HttpTracker` request log (driver)

- The existing `HttpTracker` (idleness) gains an opt-in request log, enabled with `configure` `httpLog: true` (sent only with `--trace`; every session sends it explicitly so a leftover app process stops logging). Disabled, the tracker runs exactly as before.
- **Chaining.** The tracker wraps the `HttpOverrides` that was current at binding init. Apps (Sentry, Dio setups, certificate pinning) often set `HttpOverrides.global` later in `main()`, which used to hide their traffic. While the log is enabled, the driver checks at every RPC boundary whether its override is still outermost and, if not, wraps the app's override with a **log-only** layer: it chains to the app's `createHttpClient`, never adds idle waiting (settle behavior is unchanged), and skips clients that an inner Tapwing layer already wraps, so nothing is logged twice. Clients created before a re-wrap, and native HTTP stacks (`cupertino_http`, `cronet_http`), are not seen. This is a documented limit.
- **Privacy (mandatory).** No request or response bodies. URLs keep scheme, host, port, and path. User info and fragments are dropped. Query values are kept only for keys that are not sensitive and values that do not look like tokens; others become `<redacted>`. Path segments that look like tokens (JWTs, long hex or base64 runs) are redacted too. Headers: at most 24 per direction, values capped at 200 chars; values of `Authorization`, `Proxy-Authorization`, `Cookie`, `Set-Cookie`, `X-Api-Key`, and any name containing `token`, `secret`, `auth`, `session`, `password`, `signature`, `csrf`, `xsrf`, or `key` are redacted. Error messages have query strings stripped. The CLI applies the same header and query redaction again before writing, as defense in depth.
- **Bounded.** A ring buffer of 500 finished entries plus a `dropped` counter, as in F7. An entry is finished when its response body completes, it errors, it is aborted or upgraded, or the existing safety timeout releases it.
- **Attribution and delivery.** This uses the F7 mechanism. Each entry carries the RPC request id in flight when the request **started** (the action that caused it). Finished entries piggyback on every response as `http: [...]` with `droppedHttp`. The `httpLog` RPC (`since: <seq>`) re-reads entries after native-lane steps and at flow end, and also returns requests still in flight. The CLI splits entries by step boundary exactly like issues (requests started by the previous step but finished later go to that step). Wire changes are additive: older drivers ignore `httpLog` and never confirm it, so the trace reports `http: "unsupported"`. No protocol bump is needed.

#### Viewer (`trace.html`)

A single self-contained file with no external URLs, inline CSS and vanilla JS, and the F8a palette
(light/dark via `prefers-color-scheme`, usable at phone width). The trace JSON is embedded in a
`<script type="application/json">` block with `<`, `>`, `&`, U+2028, and U+2029 escaped. The JS
renders data with `textContent` only and accepts image sources only as `data:image/` URIs, so text,
URLs, and headers from the app cannot inject markup. Per flow it shows a proportional timeline bar,
then a step list: status, duration, lane and attempt badges, and an expandable detail with error,
app issues, HTTP table, screenshot, and a collapsible UI tree. Filters: all / failed / app health
issues / HTTP errors (status ≥ 400 or a non-`completed` state), plus free-text search.

#### Acceptance criteria

| Criterion | Status |
|---|---|
| `--trace` writes `trace.json` (`tapwing.trace/1`) + offline `trace.html` per shard, with no collisions under sharding or `--flatten-debug-output` | **Met** (`f8_trace.rs`: two shards in one flattened dir write `trace-shard-1/2.*`, assets prefixed `s1_`/`s2_`) |
| Steps carry timing, lane, depth, attempt, optional status, F7 issues, failure screenshot and tree; `--trace-screenshots steps` adds per-step screenshots and trees | **Met** (`f8_trace.rs`; live on both platforms, incl. `lane: hybrid` for a system-dialog tap) |
| HTTP log: chaining to a previously installed and to a later-installed `HttpOverrides`, no double logging, redaction, ring buffer with dropped count, connection errors, in-flight at flow end | **Met** (`http_tracker_test.dart`, `http_log_rpc_test.dart`, `driver.rs` wire test, `f8_trace.rs`; live: the sample app's later-installed override applied and each request logged once, 0 secrets in the output) |
| HTML: zero external URLs, XSS-safe for flow text, URLs, and headers, filters, light/dark, small screens | **Met** (`trace/html.rs` tests: hostile `</script>` payload round-trips inside the JSON block, template has no URL, no `innerHTML`; checked rendered at 390 and 900 px, light and dark) |
| Without `--trace`: no extra driver calls, no new files, unchanged `run.json` | **Met** (`f8_trace.rs`: 0 trace calls, identical step keys; live: no output dir created) |
| Live: sample app flow on the iOS Simulator and an Android emulator; flow still passes in Maestro 2.12.1; per-step overhead `--trace` on/off and HTML size recorded | **Met:** iPhone 18 Pro +0.35 ms/step, `emulator-5630` −3.7 ms/step (noise), `trace.html` ~28 KB (13 steps) and ~330–350 KB with `--trace-screenshots steps`; Maestro 2.12.1 PASS on both. See [bench/results/f8b/summary.md](../../bench/results/f8b/summary.md) |

## 9. Risks

| Risk | Mitigation |
|---|---|
| In-app driver required (Maestro needs none) | Dev dependency + generated entrypoint; black-box fallback via native lane |
| In-process input bypasses OS (won't catch IME/autocorrect/engine-input bugs) | Input crosses the engine boundary (closer to reality than direct callbacks); `--input native` for full OS fidelity |
| Accessibility string composition drift → regex mismatches | Differential test from F0; `strict` mode emulates `AccessibilityBridge` |
| Tighter idle detection exposes flows that only passed in Maestro due to slowness | `compat.settle: maestro` (emulates Maestro's settle windows), clear reporting |
| Conflicts with custom bindings (Sentry…) | Mixin applied last (proven by Marionette) |
| Fast Maestro 2.x release cadence | Parser tested against `FlowCommandSchema` and fixtures; CI job on new tags |
| White-box habits leaking into tests | Opt-in APIs off by default, reports flag non-black-box runs, differential CI run in Maestro |
| F7 error hooks clash with crash reporters (Crashlytics, Sentry) replacing `FlutterError.onError` | Always chain to the previous handler, re-wrap at RPC boundaries, chaining test in acceptance criteria |
| Overflow reporting is compiled out of profile builds | Resolved by the §8.1 spike: read-only `OverflowDetector`, debug-parity tests, ~0.1 ms per scan |

## Sources

- Maestro source (Apache 2.0), commit `4413a33` (2026-11-17): `YamlFluentCommand.kt`, `YamlElementSelector.kt`, `Orchestra.kt`, `Maestro.kt`, `Filters.kt`, `AndroidDriver.kt`, `GraalJsEngine.kt`, `WorkspaceConfig.kt`, `maestro_android.proto`, `maestro-test` (177 fixtures, 180 tests)
- Flutter SDK 3.47.5 (local): `gestures/binding.dart`, `services/binding.dart`, `services/text_input.dart`, `services/system_channels.dart`, `scheduler/binding.dart`, `painting/image_cache.dart`
- [Maestro – Commands](https://docs.maestro.dev/reference/commands-available) · [Selectors](https://docs.maestro.dev/api-reference/selectors)
- [Maestro blog: Making Maestro work better with Flutter](https://www.maestro.dev/blog/the-power-of-open-source-making-maestro-work-better-with-flutter)
- [Maestro flakiness – source code analysis (dev.to)](https://dev.to/omnarayan/maestro-flakiness-source-code-analysis-13ng)
- [Patrol 4.0](https://patrol.leancode.co/v4) · [Patrol README](https://github.com/leancodepl/patrol/blob/master/README.md)
- [marionette_flutter](https://pub.dev/packages/marionette_flutter) · [marionette_mcp](https://pub.dev/packages/marionette_mcp)
- [Fluttium](https://fluttium.dev/)
- [adb_client (Rust)](https://docs.rs/crate/adb_client/2.1.4/source/README.md)
- [serde_yaml deprecation discussion](https://users.rust-lang.org/t/serde-yaml-deprecation-alternatives/108868)
- [JS engine benchmark (Boa vs QuickJS)](https://dev.to/ahaoboy/js-engine-benchmark-2025-7-8-163b)

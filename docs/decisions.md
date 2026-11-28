# Design Decisions

Summary of key architectural trade-offs and compatibility choices made in Tapwing. Rationale from earlier milestones is preserved in [archive/plan-history.md](archive/plan-history.md), and guiding constraints live in [STRATEGY.md](../STRATEGY.md).

## 1. Single Rust CLI vs JVM Runner

Tapwing replaces Maestro's Kotlin/JVM CLI with a compiled Rust binary (`crates/tapwing`). Eliminating JVM startup reduces cold flow parsing from ~800 ms to ~3–4 ms (see [PARSE_RESULTS.md](../bench/PARSE_RESULTS.md)) and lets a single self-contained binary manage device processes, TCP/gRPC/HTTP transports, QuickJS-ng evaluation, and HTML/JUnit reports without requiring Java or Maestro on the user's machine.

## 2. Black-Box YAML + `dev_dependency` Driver vs In-Dart Test Frameworks

Rather than asking teams to rewrite tests in Dart (`flutter_test` / `integration_test` / Patrol), Tapwing keeps tests as external Maestro YAML flows and isolates `tapwing_driver` as a `dev_dependency` invoked from a separate `lib/main_tapwing.dart` entrypoint created by `tapwing init`.

- Production `main.dart` and production widgets do not need test instrumentation code.
- In release builds the driver refuses to start unless `TAPWING_ALLOW_RELEASE=true` is explicitly compiled in, avoiding exposed local sockets in shipped binaries.
- Apps without `tapwing_driver` still run through the bundled native lanes (`--input native` or automatic fallback).

## 3. Framework Semantics Parity Over Synthetic Dart Locators

`tapwing_driver` derives hierarchy trees and coordinates from Flutter's platform semantics contract (physical pixels and `AccessibilityBridge` composition on Android; logical points and `IOSDriver.mapViewHierarchy` rules on iOS Simulator) rather than inspecting arbitrary internal widget types. This preserves 1:1 selector behavior with `maestro hierarchy` and guarantees that flows verified in Tapwing match standard accessibility semantics.

## 4. Bundled Owned Native Drivers (Apache-2.0)

Native lanes are implemented directly in `drivers/android` and `drivers/ios` (ported from Maestro's Apache-2.0 drivers with unnecessary routes and third-party Swift dependencies removed) and bundled into release tarballs with `.sha256` sidecars verified at runtime (`driver_bundle.rs`). Removing Maven/GitHub runtime downloads keeps CI runs offline-safe and allows Tapwing-specific reliability improvements:

- iOS `RequestGate` deadlines and `GET /status` session IDs so wedged XCUITest calls are detected and restarted cleanly (`docs/native-lanes.md`).
- Shared Rust `native_core` (`recovery.rs`, `ops.rs`) enforcing bounded restarts (up to 3 restarts within time budgets) and safe retry rules (read ops retry automatically; mutating input retries only when it provably never left the host).

## 5. Embedded QuickJS-ng Engine

`runScript` and `${...}` JavaScript expressions run in embedded QuickJS-ng (`crates/tapwing/src/js/`) rather than spawning Node or Boa. It starts fast, preserves `output`, `maestro`, `json()`, `relativePoint()`, synchronous `http.*`, and `faker`, stops immediately on `SIGINT`/`SIGTERM`, and enforces a 1 GiB memory cap so runaway scripts cannot crash the runner process.

## 6. Post-Settle `OverflowDetector` for Profile Builds

Tapwing recommends Android `profile` builds for realistic performance and reliable input timing, where Flutter compiles out debug `RenderFlex` overflow messages. `OverflowDetector` (`packages/tapwing_driver/lib/src/diagnostics/overflow_detector.dart`) inspects painted render boxes after settle so `layoutOverflow` issues are still caught in profile and release builds alongside debug mode.

## 7. App Health Default: `report` Pre-1.0, `fail` at 1.0

Today (`0.x`), `--app-errors` defaults to `report` so teams migrating green suites from Maestro immediately see overflows, exceptions, HTTP load issues, and crashes without sudden CI breakage. Per [STRATEGY.md](../STRATEGY.md), from `1.0` onward app health issues are intended to fail flows by default unless suppressed via `config.yaml` or CLI flags.

## 8. Feature-Gating Test Doubles (`test-support`)

`FakeDriver` and fake-driver suite helpers are compiled only under `#[cfg(any(test, feature = "test-support"))]` (`crates/tapwing/src/fake_driver.rs`), keeping production release binaries lean while keeping deterministic orchestrator integration tests fast in `crates/tapwing/tests/integration/`.

## 9. Fast Inner-Loop Extensions (Opt-In, Gray-Box Flagged)

Tapwing speeds up AI and human test-authoring iteration while strictly separating gray-box shortcuts from clean black-box runs (per [STRATEGY.md](../STRATEGY.md)):

- **In-app hooks (`callHook` + `TapwingBinding.ensureInitialized(hooks: ...)` / `registerHook`):** Dev/test entrypoints (such as `lib/main_tapwing.dart`) may register named synchronous or asynchronous Dart callbacks `(BuildContext context, Map<String, Object?> args)`. Tapwing resolves `context` from the active visible route (skipping `Offstage` / disabled `TickerMode` subtrees), so `Navigator`, `GoRouter`, `Provider`, and `Bloc` work with zero changes to production `lib/main.dart`. Hooks are **blocked by default** unless `config.yaml` sets `tapwing.allowHooks: true` or the caller passes `--allow-hooks` (and `--no-hooks` always overrides `config.yaml`). Policy denials cannot be bypassed with `optional: true`. Hook completion triggers a reactive post-hook settle by default and binds any returned JSON-encodable payload to `output.<outputVariable>`.
- **Daemon-free warm attach (`--attach`) and partial step resume (`--start-from <step|label>`):** Because `TapwingServer` already re-dials periodically (150 ms for the first 60 s, 1 s thereafter) and re-reads `tmp/tapwing_port` on iOS Simulator / `adb reverse` on Android before every dial, `--attach` attaches directly to a running process with no background daemon, drains stale `appIssues`/`httpLog` watermarks, and converts a leading `launchApp` into `warm_attach` (with a warning if `clearState`/`clearKeychain` was skipped). `--start-from` resumes from a 1-based top-level step or exact `label:` while preserving header `env:` (`defineVariables`) and original 1-based step numbering, skipping `onFlowStart` when earlier steps are skipped.
- **Explicit gray-box accountability:** Whenever `callHook`, `--attach`, or `--start-from` activates, Tapwing announces it on `stderr` and flags the flow across JSON (`graybox`, `grayboxReasons`, `hooksUsed`, `hookCalls`, `partialRun`, `warmAttach`), HTML / detailed HTML reports, JUnit (`tapwing.*` properties and `<system-err>`), and Trace JSON/HTML (`trace.json` / `trace.html`). Clean runs omit all gray-box fields.
- **Deferral of host-side state checkpoints (`saveState` / `restoreState`):** Evaluated and intentionally **deferred**:
  - **Android:** `adb shell run-as` is blocked on non-debuggable `profile` (and `release`) builds, and Android Keystore / Keystore2 keys, runtime permissions, and SystemUI state live outside `/data/data/<pkg>`, causing app-state split-brain on raw tarball restores.
  - **iOS Simulator:** `cfprefsd` caches `NSUserDefaults` in RAM, Keychain entries live in `securityd` outside the app data container, App Group containers live separately, and restoring safely still requires terminating and cold-relaunching the app (~1–2 s). In-app Dart `callHook` + warm `--attach` achieve sub-second deterministic seeding across Android and iOS without split-brain OS state.

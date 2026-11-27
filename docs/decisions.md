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

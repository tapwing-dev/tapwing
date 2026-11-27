# Architecture

Tapwing executes [Maestro](https://maestro.dev) YAML flows against Flutter apps using a single Rust CLI binary paired with an in-app Dart driver, plus bundled native drivers when UI lives outside Flutter.

## Two-Lane Execution Model

```text
  flow.yaml ──▶ tapwing CLI (Rust)
                   │
                   ├──▶ tapwing_driver (in-app Dart over TCP loopback / adb reverse)
                   │       Semantics snapshot, selector matching, reactive idle & input
                   │
                   └──▶ native lanes (bundled, on demand)
                           ├── Android: dev.tapwing.driver (gRPC over adb forward)
                           └── iOS Simulator: dev.tapwing.ios-runner (HTTP/1.1 localhost)
```

In `--input auto` (the default), Tapwing routes normal app interactions through `tapwing_driver` and switches to the native lane only when focus leaves the Flutter view (permission dialogs, system settings, WebViews, notifications, hardware keys) or when an app is built without `tapwing_driver`. Control returns to the fast Flutter path as soon as the app regains focus. See [Native lanes](native-lanes.md) for routing rules and recovery limits.

## Rust CLI Layout (`crates/tapwing/src/`)

| Module | Responsibility |
|---|---|
| `main.rs`, `cli_args.rs` | CLI parsing (`clap`), `@argfile` expansion, signal-aware process exit codes (`0`, `1`, `2`, `130`, `143`) |
| `flow/` | Maestro 2.12.1 YAML parser (`yaml.rs`), command decoding (`decode.rs`), IR (`ir.rs`, `commands.rs`), sub-flow cycle detection |
| `workspace/`, `shard.rs` | Workspace discovery, `config.yaml` planning (`flows`, `executionOrder`, tag filters), round-robin & redundant sharding |
| `init/` | `tapwing init` onboarding (`pubspec.yaml` editing preserving comments, `lib/main_tapwing.dart` entrypoint, zone checks) |
| `orchestra/` | Flow interpreter (`mod.rs`), command dispatch (`dispatch.rs`), JS expression evaluation (`eval.rs`), hints (`describe.rs`, `diagnostics.rs`) |
| `js/` | Embedded QuickJS-ng runtime (`mod.rs`), synchronous `http.*` client (`http.rs`), `faker` (`faker.rs`) |
| `backend.rs`, `driver.rs` | `FlowDriver` trait, `DriverPlatform`, host TCP protocol client to `tapwing_driver` |
| `ui_tree.rs` | Shared hierarchy node model (`TreeNode`), element bounds math, selector matching, and pruning helpers |
| `driver_probe/` | Pre-`launchApp` probe (`android.rs`, `ios.rs`, `markers.rs`, `scan.rs`) and install cache (`cache.rs`) detecting whether an installed build contains `tapwing_driver` |
| `run/` | Hybrid Flutter/native routing (`hybrid.rs`), app launch & port handoff (`launch.rs`), driver-probe coordination (`driver_detect.rs`), Flutter ops (`flutter_ops.rs`), multi-shard runner (`suite.rs`, `shard_outcome.rs`) |
| `device/` | Device discovery and host commands for Android (`adb_client/`, `android/`) and iOS Simulator (`ios.rs` via `simctl`) |
| `native_core/` | Shared native lane runtime: cancellation signals, black-box app tracking, handle management (`mod.rs`), bounded reconnect/restart (`recovery.rs`), and element/input operations (`ops.rs`) |
| `native_lane/` | Android native lane: bundled `dev.tapwing.driver` APK management (`tapwing_apk.rs`, `apk.rs`), gRPC client (`client.rs`), hierarchy/ops, session restart |
| `ios_native_lane/` | iOS Simulator native lane: bundled `dev.tapwing.ios-runner` runner (`tapwing_driver.rs`), `xcodebuild` session & orphan cleanup (`session.rs`, `orphans.rs`), HTTP/1.1 client, hierarchy/ops |
| `app_health.rs`, `failure.rs`, `trace/`, `report/`, `screenshot.rs` | Overflow/error/crash attribution, "Did you mean" failure capture, `--trace` HTML/JSON export, JUnit/HTML/JSON reports, image comparison |
| `driver_bundle.rs`, `interrupt.rs`, `process.rs`, `atomic_write.rs` | Bundled driver `.sha256` verification, `SIGINT`/`SIGTERM` RAII teardown, bounded subprocess helpers, atomic file writes |
| `fake_driver.rs` | In-memory test double for the orchestrator (gated behind `#[cfg(any(test, feature = "test-support"))]`) |

## In-App Flutter Driver (`packages/tapwing_driver/`)

When initialized via `TapwingBinding.ensureInitialized()` (generated in `lib/main_tapwing.dart` without touching `main.dart`):

- **Port handoff:** The CLI picks a free host TCP port and passes `--dart-define` / launch environment (or `tmp/tapwing_port` on iOS Simulator); on Android it sets up `adb reverse`. The app dials the CLI loopback server and keeps one persistent newline-delimited JSON-RPC session. In release builds the driver stays disabled unless built with `--dart-define=TAPWING_ALLOW_RELEASE=true`.
- **Snapshot & selectors (`src/snapshot/`, `src/selector/`):** Walks Flutter's live `SemanticsNode` tree in traversal order, emitting Maestro-compatible attributes for Android (`AccessibilityBridge` pixels) and iOS Simulator (points, `label`/`value`), while caching semantics hashes (`SemanticsHashCache`) so idle paint frames short-circuit in O(1).
- **Reactive settle (`src/idle/`):** Combines frame/ticker activity (`IdleDetector`), transient vs endless animation classification (`AnimationScanner`), platform channel traffic (`ChannelTracker`), bounded `dart:io` HTTP waiting (`HttpTracker`), and soft-keyboard settling (`KeyboardWaiter`) instead of blind sleeps.
- **Input & diagnostics (`src/input/`, `src/diagnostics/`):** Dispatches gestures through Flutter's engine boundary (`GestureBinding`), injects Unicode text deltas, captures `FlutterError.onError` / `PlatformDispatcher.onError` / post-settle `OverflowDetector` issues (`app_issues.dart`), and logs redacted HTTP metadata when `--trace` is enabled (`http_log.dart`).

## Bundled Native Drivers (`drivers/`)

- **Android (`drivers/android/`):** Single Kotlin/Java UiAutomator APK (`dev.tapwing.driver`) implementing Tapwing's gRPC schema (`crates/tapwing/proto/maestro_android.proto`), including offline-tested IME restore helpers (`ImeRestore.kt`) for Unicode text entry without leaving the device's keyboard switched. See [drivers/android/README.md](../drivers/android/README.md).
- **iOS Simulator (`drivers/ios/`):** Zero-dependency Swift + minimal Objective-C bridge (`TapwingRunner`) serving a 14-route keep-alive HTTP/1.1 API (`Core/` logic testable via `swift test`, `UITests/` XCUITest runner), with `RequestGate` deadlines and `/status` session verification so wedged XCUITest queries fail cleanly and can be restarted cleanly. See [drivers/ios/README.md](../drivers/ios/README.md).

## Historical Context

Pre-release design logs and per-phase milestones from initial development are preserved in [archive/plan-history.md](archive/plan-history.md). Project principles and scope boundaries live in [STRATEGY.md](../STRATEGY.md).

# Roadmap

Tapwing's priorities follow [STRATEGY.md](../STRATEGY.md): deliver fast, faithful Maestro execution for Flutter first, expand to React Native next, and add physical iOS device support after React Native.

## Current State (`0.x` Pre-1.0)

Shipped and verified in the repository today:

- Maestro 2.12.1 YAML parser parity, QuickJS-ng scripting, workspace planning, and multi-device sharding (`tapwing check`, `tapwing test`, `tapwing hierarchy`, `tapwing init`).
- In-app Flutter driver (`packages/tapwing_driver`) supporting Flutter `>= 3.41`, reactive settle, Android pixels + iOS Simulator points parity, app health diagnostics (`FlutterError`, `PlatformDispatcher`, profile `OverflowDetector`, crash logs), and `--trace` offline HTML viewer with redacted HTTP logs.
- Bundled Apache-2.0 Android UiAutomator (`drivers/android`) and iOS Simulator XCUITest (`drivers/ios`) native drivers with hybrid auto-switching, platform-view fast fallback, bounded recovery, and clean `SIGINT`/`SIGTERM` teardown.
- Release packaging (`scripts/dist.sh`, `scripts/install.sh`, Homebrew tap formula generation, macOS + Linux builds in `.github/workflows/release.yml`).

## Planned Priorities

| Priority | Initiative | Scope & Notes |
|---|---|---|
| 1 | **Flutter 1.0 hardening** | Stabilize CLI flags, JSON/trace schemas, real-world app compatibility (WebViews, deep links, complex overlays), and flip default `--app-errors` from `report` to `fail` at `1.0` ([STRATEGY.md](../STRATEGY.md)). |
| 2 | **React Native & Expo driver** | In-app React Native driver sharing Tapwing's Rust runner, Maestro YAML parser, native-lane fallback, and diagnostics. Explicitly prioritized **before** physical iOS devices per [STRATEGY.md](../STRATEGY.md). |
| 3 | **Physical iOS devices** | Device-tunnel / XCUITest support for real iOS hardware once Flutter hardening and React Native support are in place. |

## Under Evaluation (Unprioritized)

Ideas evaluated against Tapwing's vision that are not yet scheduled:

- **WSL Android forwarding:** Supporting `adb` across WSL2 to a Windows host daemon if reliable reverse-tunnel behavior can be guaranteed without flaky port heuristics.
- **Agent / MCP runner interface:** Structured step-by-step control (`tapwing` binary already emits structured JSON/trace outputs) only if it serves black-box verification without turning Tapwing into an AI test generator.

## Explicit Non-Goals

Per [STRATEGY.md](../STRATEGY.md), Tapwing intentionally avoids:

- **Dart-only test authoring DSLs:** Tests stay external Maestro YAML flows, not Dart widget/integration test suites.
- **AI assertion engines (`assertWithAI`, etc.):** Commands requiring Maestro Cloud continue to parse cleanly and fail clearly at runtime.
- **Hosted execution services or device farms (e.g., AWS Device Farm adapters):** Tapwing focuses on running reliably as a local and CI CLI (`tapwing test --shard-split` / `--format junit`).

# Contributing

Everything you need to build Tapwing from source and run its tests. For what Tapwing does and how to use it, see the [README](README.md). For design decisions, see [PLAN.md](PLAN.md).

## Repository layout

| Path | What lives there |
|---|---|
| `crates/tapwing` | The Rust CLI and library: `init/`, flow parser (`flow/`), workspace and sharding planners (`workspace/`, `shard.rs`), orchestrator (`orchestra.rs`), QuickJS-ng engine (`js.rs`), driver trait and `FakeDriver` (`backend.rs`, `fake_driver.rs`), device backends (`device/`: adb client, Android host commands, iOS Simulator over `simctl`), native lanes (`native_lane/` for Android, `ios_native_lane/` for iOS), the driver probe that tells before `launchApp` whether the installed build holds the driver (`driver_probe/`), the hybrid and multi-shard runner (`run/`, `driver.rs`), reports (`report/`), app health (`app_health.rs`), failure diagnostics (`failure.rs`) and the trace viewer (`trace/`) |
| `packages/tapwing_driver` | The in-app Dart driver: Semantics tree snapshot, Maestro selector engine, idle detector, input injection. See its [README](packages/tapwing_driver/README.md) |
| `drivers/` | Tapwing's own native drivers: the Android UiAutomator APK ([drivers/android](drivers/android/README.md)) and the iOS Simulator XCUITest runner ([drivers/ios](drivers/ios/README.md)) |
| `compat/` | Parser compatibility suite: 463 Maestro fixtures and 81 custom probes with their reference Maestro IR. See [compat/README.md](compat/README.md) |
| `examples/sample_app` | A sample Flutter app (Android and iOS) with Maestro flows in `.maestro/` (iOS variants in `.maestro/ios/`) |
| `bench/` | Benchmarks against Maestro, differential and hierarchy scripts, and recorded results |
| `scripts/` | `dist.sh` (release tarball), `install.sh` (installer), `build-android-driver.sh`, `build-ios-driver.sh`, `test-dist.sh` (tests of the release scripts) |
| `docs/` | User guides linked from the README |
| `.agents/skills/` | Workspace agent skills: Rust modularity, systems reliability, Dart driver invariants, Maestro parity, documentation curation |

## Build and run from source

```sh
# 1. Build the sample app with the driver (dev dependency + separate entrypoint, production main.dart unchanged)
cd examples/sample_app
flutter build apk --profile --target-platform android-arm64 -t lib/main_tapwing.dart
adb install -r build/app/outputs/flutter-apk/app-profile.apk

# 2. Build the CLI
cd ../../crates/tapwing && cargo build --release

# 3. Run a flow, or a whole workspace like `maestro test .maestro/`
./target/release/tapwing test ../../examples/sample_app/.maestro/login_and_browse.yaml --report run.json
./target/release/tapwing test ../../examples/sample_app/.maestro --include-tags smoke

# Dump the UI hierarchy in Maestro attributes (text, accessibilityText, hintText, resource-id, bounds)
./target/release/tapwing hierarchy

# Settle diagnostics: verify no significant animation is still running after each action
./target/release/tapwing test flow.yaml --check-settle [--settle-policy v2|legacy]
```

For the iOS Simulator, see [docs/devices.md](docs/devices.md). The native drivers are built by `scripts/build-android-driver.sh` (JDK 17+, Android SDK platform 37) and `scripts/build-ios-driver.sh` (Xcode).

## Tests

Every suite below runs in CI on each pull request (see [Continuous integration](#continuous-integration)); run the ones you touched before you push.

```sh
# Rust CLI and library: unit, integration, CLI end-to-end and doc tests (needs protoc)
cd crates/tapwing && cargo fmt --check && cargo clippy --locked --all-targets -- -D warnings && cargo test --locked -p tapwing

# Parser users and shipped flows
cargo check --manifest-path bench/parse/Cargo.toml --locked
crates/tapwing/target/debug/tapwing check examples/sample_app/.maestro bench/f11a/flows

# Dart: the in-app driver, then the sample app (widget tests, selector compatibility, flow ids)
cd packages/tapwing_driver && flutter pub get && flutter analyze && flutter test
cd examples/sample_app && flutter pub get && flutter analyze && flutter test

# Native drivers (each builds its release artifact and SHA-256 after the unit tests)
scripts/build-android-driver.sh        # JDK 17+, Android SDK platform 37: Kotlin unit tests + APK
scripts/build-ios-driver.sh            # macOS with Xcode: Swift unit tests + simulator runner

# Release and installer scripts
scripts/test-dist.sh
```

| Suite | Where | What it guards |
|---|---|---|
| Unit tests | `crates/tapwing/src/**` (`#[cfg(test)]`) | Driver connection and reconnect handling, the `launchApp` / `setPermissions` scripts, the driver-probe scripts and the Android native-lane startup against a fake ADB server (`device/adb_client/fake.rs`; every script is parsed with `sh -n` and run under the host `sh` with stubbed device commands, the probe's over real zip fixtures with the host's `unzip` and `grep`), driver artifact discovery and SHA-256 checks, reports, trace redaction, planners |
| CLI end-to-end | `crates/tapwing/tests/cli_e2e.rs` | The compiled `tapwing` binary: `--version`/`--help`, `check`, `init`, `test` argument validation, exit codes and messages |
| Integration | `crates/tapwing/tests/*.rs` | Parser parity with Maestro (`compat`), workspace planner, sharding, reports (`report_edge_cases`), app health, failure reports, trace, `init` |
| Dart driver | `packages/tapwing_driver/test` | Selector engine, snapshot semantics, RPC protocol and error wire format, idle detection, input injection |
| Sample app | `examples/sample_app/test` | Widget tests of the app, `tapwing_driver` selectors resolving against the real UI, and every `id:` in `.maestro/` still existing in the app |
| Android driver | `drivers/android/driver/src/test` | Pure Kotlin helpers: ASCII key table, Unicode text chunks and splices, the cancellable typing loop, XML character filtering |
| iOS driver | `drivers/ios/Tests` | `TapwingRunnerCore`: HTTP/1.1 framing, wire models, error classification, orientation math |

Useful single Rust suites: `cargo test --test cli_e2e`, `--test compat` (parser parity with Maestro), `--test workspace` (workspace planner parity), `--test f6_sharding_and_reports`, `--test init` (also keeps the CLI and driver versions in lockstep).

Tests never need a device: anything that talks to an emulator, simulator or a real app (`tapwing test` against the sample app, the benchmarks) is run by hand, not in PR CI. Keep new tests deterministic: bind loopback servers once and pass the live listener, never sleep to wait for another thread, and prefer a test seam over timing (see `.agents/skills/rust-systems-reliability`).

## Continuous integration

[`.github/workflows/ci.yml`](.github/workflows/ci.yml) runs on every pull request and every push to `main`, calling one reusable workflow per layer:

| Job | Workflow | Runs |
|---|---|---|
| `Rust` (`ubuntu-latest`, `macos-latest`) | [`ci-rust.yml`](.github/workflows/ci-rust.yml) | `cargo fmt --check`, `clippy -D warnings`, `cargo test --locked -p tapwing`, `bench/parse` check, `tapwing check` over the sample app and bench flows |
| `Dart & Sample App` (`Flutter 3.47.5`, `3.50.0`) | [`ci-dart.yml`](.github/workflows/ci-dart.yml) | `flutter analyze` and `flutter test` in `packages/tapwing_driver` and `examples/sample_app` |
| `Android driver` (`drivers/android`) | [`ci-android-driver.yml`](.github/workflows/ci-android-driver.yml) | `scripts/build-android-driver.sh` (Kotlin unit tests, release APK, SHA-256) |
| `iOS driver` (`drivers/ios`) | [`ci-ios-driver.yml`](.github/workflows/ci-ios-driver.yml) | `scripts/build-ios-driver.sh` (Swift unit tests, simulator runner build, SHA-256) |
| `Release scripts` (`ubuntu-latest`, `macos-latest`) | [`ci-scripts.yml`](.github/workflows/ci-scripts.yml) | `scripts/test-dist.sh` |
| `CI Gate` | [`ci.yml`](.github/workflows/ci.yml) | Succeeds only when every job above succeeded; this is the one check `main` requires |

## Benchmarks

```sh
bench/run_bench.sh 3 <serial>          # against Maestro, results in bench/results/<serial> or OUT=...
bench/parse/run_parse_bench.sh         # parser against Maestro, no device required (bench/PARSE_RESULTS.md)
```

When you cite a timing, name the device, the build mode and the baseline (for example `emulator-5554`, profile APK, Maestro).

## Documentation

Keep the README short and runnable; put reference material in `docs/`. Every documented subcommand, flag and path must match the code: compare against the `clap` structs in `crates/tapwing/src/main.rs`, and run `tapwing check examples/sample_app/.maestro/` after touching a flow example.

# Contributing

Everything you need to build Tapwing from source and run its tests. For what Tapwing does and how to use it, see the [README](README.md). For design rationale, see [docs/architecture.md](docs/architecture.md) and [docs/decisions.md](docs/decisions.md).

## Repository layout

| Path | What lives there |
|---|---|
| `crates/tapwing` | The Rust CLI and library: `init/`, flow parser (`flow/`), workspace and sharding planners (`workspace/`, `shard.rs`), orchestrator (`orchestra/`), QuickJS-ng engine (`js/`), shared UI tree matching (`ui_tree.rs`), driver trait and `test-support`-gated `FakeDriver` (`backend.rs`, `fake_driver.rs`), device backends (`device/`: adb client, Android host commands, iOS Simulator over `simctl`), native lanes (`native_core/`, `native_lane/` for Android, `ios_native_lane/` for iOS), the driver probe that tells before `launchApp` whether the installed build holds the driver (`driver_probe/`), hybrid and multi-shard runner (`run/`, `driver.rs`), reports (`report/`), app health (`app_health.rs`), failure diagnostics (`failure.rs`), trace viewer (`trace/`), and integration tests (`tests/integration/`) |
| `packages/tapwing_driver` | The in-app Dart driver: Semantics tree snapshot, Maestro selector engine, idle detector, input injection. See its [README](packages/tapwing_driver/README.md) |
| `drivers/` | Tapwing's own native drivers: the Android UiAutomator APK ([drivers/android](drivers/android/README.md)) and the iOS Simulator XCUITest runner ([drivers/ios](drivers/ios/README.md)) |
| `compat/` | Parser compatibility suite: 463 Maestro fixtures and 81 custom probes with their reference Maestro IR. See [compat/README.md](compat/README.md) |
| `examples/sample_app` | A sample Flutter app (Android and iOS) with Maestro flows in `.maestro/` (iOS variants in `.maestro/ios/`) |
| `bench/` | Benchmarks against Maestro, differential and hierarchy scripts, and recorded results |
| `scripts/` | `dist.sh` (release tarball), `install.sh` (installer), `release.sh` (two-step release automation), `build-android-driver.sh`, `build-ios-driver.sh`, `test-dist.sh` (tests of release and dist scripts), `ci-android-e2e.sh` (the sample app's Android flows on one adb device, used by CI and locally) |
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

# End to end on an Android emulator or device: builds the CLI, the driver APK and the sample app, runs the flows
scripts/ci-android-e2e.sh --serial emulator-5554
```

| Suite | Where | What it guards |
|---|---|---|
| Unit tests | `crates/tapwing/src/**` (`#[cfg(test)]`) | Driver connection and reconnect handling, the `launchApp` / `setPermissions` scripts, the driver-probe scripts and the Android native-lane startup against a fake ADB server (`device/adb_client/fake.rs`; every script is parsed with `sh -n` and run under the host `sh` with stubbed device commands, the probe's over real zip fixtures with the host's `unzip` and `grep`), driver artifact discovery and SHA-256 checks, reports, trace redaction, planners |
| CLI end-to-end | `crates/tapwing/tests/integration/cli_e2e.rs` | The compiled `tapwing` binary: `--version`/`--help`, `check`, `init`, `test` argument validation, exit codes and messages |
| Integration | `crates/tapwing/tests/integration/` | Parser parity with Maestro (`compat`), workspace planner, sharding, reports (`report_edge_cases`), app health, failure reports, trace, `init` |
| Dart driver | `packages/tapwing_driver/test` | Selector engine, snapshot semantics, RPC protocol and error wire format, idle detection, input injection |
| Sample app | `examples/sample_app/test` | Widget tests of the app, `tapwing_driver` selectors resolving against the real UI, and every `id:` in `.maestro/` still existing in the app |
| Android driver | `drivers/android/driver/src/test` | Pure Kotlin helpers: ASCII key table, Unicode text chunks and splices, the cancellable typing loop, XML character filtering |
| iOS driver | `drivers/ios/Tests` | `TapwingRunnerCore`: HTTP/1.1 framing, wire models, error classification, orientation math |

Useful single Rust suites (inside `crates/tapwing/tests/integration`): `cargo test --test integration cli_e2e::`, `--test integration compat::` (parser parity with Maestro), `--test integration workspace::` (workspace planner parity), `--test integration f6_sharding_and_reports::`, `--test integration init::` (also keeps the CLI and driver versions in lockstep).

The suites above never need a device. The Android emulator run of the sample app's flows (`scripts/ci-android-e2e.sh`) has its own workflow, described under [Continuous integration](#continuous-integration). Everything else that talks to a device (the iOS Simulator flows, the benchmarks) is run by hand, not in PR CI. Keep new tests deterministic: bind loopback servers once and pass the live listener, never sleep to wait for another thread, and prefer a test seam over timing (see `.agents/skills/rust-systems-reliability`).

## Continuous integration

[`.github/workflows/ci.yml`](.github/workflows/ci.yml) runs on every pull request and every push to `main`, calling one reusable workflow per layer:

| Job | Workflow | Runs |
|---|---|---|
| `Rust` (`ubuntu-latest`) | [`ci-rust.yml`](.github/workflows/ci-rust.yml) | `cargo fmt --check`, `clippy -D warnings`, `cargo test --locked -p tapwing`, `bench/parse` check, `tapwing check` over the sample app and bench flows |
| `Dart & Sample App` (`ubuntu-latest`, `Flutter 3.47.5`, `3.50.0`) | [`ci-dart.yml`](.github/workflows/ci-dart.yml) | `flutter analyze` and `flutter test` in `packages/tapwing_driver` and `examples/sample_app` |
| `Android driver` (`ubuntu-latest`, `drivers/android`) | [`ci-android-driver.yml`](.github/workflows/ci-android-driver.yml) | `scripts/build-android-driver.sh` (Kotlin unit tests, release APK, SHA-256) |
| `Release scripts` (`ubuntu-latest`) | [`ci-scripts.yml`](.github/workflows/ci-scripts.yml) | `scripts/test-dist.sh` |
| `CI Gate` (`ubuntu-latest`) | [`ci.yml`](.github/workflows/ci.yml) | Succeeds only when every job above succeeded; this is the one check `main` requires |

[`ci-android-e2e.yml`](.github/workflows/ci-android-e2e.yml) is a separate workflow, the only one that runs the CLI, the Dart driver and the native driver APK together on a device. It boots an API 35 x86_64 emulator and calls `scripts/ci-android-e2e.sh`: the top-level flows of `examples/sample_app/.maestro` must pass and each flow in `.maestro/negative/` must exit with `1`. It runs on pushes to `main` and on pull requests that touch `crates/`, `packages/`, `drivers/android/`, `examples/sample_app/`, the script or the workflow, and uploads traces, reports and logcat. It is not part of `CI Gate` yet: it becomes required after a week of stable runs ([issue #3](https://github.com/tapwing-dev/tapwing/issues/3)). See [docs/ci.md](docs/ci.md#android-emulator-end-to-end-run) for the flow selection and the time limits.

`scripts/build-ios-driver.sh` (Swift unit tests, iOS Simulator XCUITest runner build, and `.sha256` generation) runs on macOS runners during releases ([`.github/workflows/release.yml`](.github/workflows/release.yml)) and can be verified locally with Xcode.

## Benchmarks

```sh
bench/run_bench.sh 3 <serial>          # against Maestro, results in bench/results/<serial> or OUT=...
bench/parse/run_parse_bench.sh         # parser against Maestro, no device required (bench/PARSE_RESULTS.md)
```

When you cite a timing, name the device, the build mode and the baseline (for example `emulator-5554`, profile APK, Maestro).

## Documentation

Keep the README short and runnable; put reference material in `docs/`. Every documented subcommand, flag and path must match the code: compare against the `clap` structs in `crates/tapwing/src/main.rs`, and run `tapwing check examples/sample_app/.maestro/` after touching a flow example.

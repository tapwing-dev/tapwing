<h1 align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="docs/brand/tapwing-logo-white.svg">
    <img alt="Tapwing" src="docs/brand/tapwing-logo-black.svg" height="56">
  </picture>
</h1>

<p align="center">
  <b>Fast, black-box end-to-end testing for Flutter apps.</b><br>
  Run your existing <a href="https://maestro.dev">Maestro</a> YAML flows up to 11× faster, with built-in crash, overflow, and failure diagnostics.
</p>

Tapwing is a single Rust binary that runs Maestro flows against Flutter apps on Android (emulators or physical devices) and the iOS Simulator. Tests stay plain YAML outside your app, while a small `dev_dependency` driver inside the app gives Tapwing exact UI synchronization so flows run fast without flaky sleeps.

## Why Tapwing?

- **Drop-in for Maestro.** Same YAML, timeouts, and selector rules (passes all 463 Maestro parser fixtures). Replace `maestro test .maestro/` with `tapwing test .maestro/`.
- **Fast.** No JVM startup and no polling. Runs the same flows 4–11× faster end-to-end than Maestro (4.1× on iOS Simulator, 7.8× on Android emulator, 11.4× on a Pixel 8a).
- **Black-box.** Flows live outside your app. The driver is a `dev_dependency` behind its own entrypoint (`lib/main_tapwing.dart`), so your production `main.dart` stays untouched.
- **Catches silent app issues.** Layout overflows, uncaught exceptions, and crashes are reported under the step that triggered them, even when all assertions pass.
- **Actionable failures.** Every failing step gives you "Did you mean" suggestions, a pruned UI tree, and a screenshot. Add `--trace` for an offline step-by-step viewer with the app's HTTP requests.
- **Handles native UI too.** Permission dialogs, WebViews, other apps, and uninstrumented builds fall back automatically to Tapwing's bundled native drivers (UiAutomator on Android, XCUITest on iOS). No Maestro install needed.

## Example flow

```yaml
# .maestro/login.yaml
appId: com.example.myapp
tags:
  - smoke
---
- launchApp:
    clearState: true
- assertVisible: "Welcome back"
- tapOn: "Email"
- inputText: "jan@example.com"
- tapOn:
    id: "password_input"
- inputText: "secret123"
- tapOn: "Log in"
- assertVisible: "Products"
```

Every standard Maestro command, selector, and `config.yaml` option works. See [docs/commands.md](docs/commands.md) for the full list.

## How it works

```
flow.yaml ──▶  tapwing (Rust CLI) ──┬──▶ tapwing_driver  inside your app: Flutter semantics + input   (fast path)
                                    └──▶ native drivers  UiAutomator / XCUITest: system UI, WebViews  (on demand)
```

Tapwing uses the in-app Flutter driver first and switches to the native driver only for UI outside Flutter, returning to the fast path once your app has focus again. Apps without `tapwing_driver` are detected automatically and driven through the native lane ([docs/native-lanes.md](docs/native-lanes.md)).

## Install

macOS (Apple Silicon or Intel) or Linux x86_64. You don't need Maestro.

```sh
# Homebrew
brew install tapwing-dev/tap/tapwing

# Or install directly (latest GitHub Release, SHA-256 verified)
curl -fsSL https://raw.githubusercontent.com/tapwing-dev/tapwing/main/scripts/install.sh | bash

tapwing --version
```

Your Flutter app needs Flutter 3.41 or newer. For installer options, upgrades, and building from source, see [docs/install.md](docs/install.md).

## Quickstart

**1. Connect the driver to your app.** This adds `tapwing_driver` under `dev_dependencies` and creates `lib/main_tapwing.dart` without modifying `main.dart`:

```sh
cd path/to/your_flutter_app
tapwing init --pub-get
```

**2. Build and install the app with the Tapwing entrypoint:**

```sh
# Android (profile build)
flutter build apk --profile -t lib/main_tapwing.dart
adb install -r build/app/outputs/flutter-apk/app-profile.apk

# iOS Simulator (debug build; the simulator has no profile mode)
flutter build ios --simulator --debug -t lib/main_tapwing.dart
xcrun simctl install booted build/ios/iphonesimulator/Runner.app
```

**3. Put a flow in `.maestro/` and validate it offline:**

```sh
tapwing check .maestro
```

**4. Run it:**

```sh
tapwing test .maestro                      # whole workspace
tapwing test .maestro/login.yaml           # single flow
tapwing test .maestro --include-tags smoke # filtered by tag
```

If one device or booted simulator is available, `--device` is optional. For flavors, monorepos, or custom entrypoints, see [docs/setup.md](docs/setup.md).

## Everyday commands

| Command | What it does |
|---|---|
| `tapwing init [DIR]` | Add `tapwing_driver` and generate `lib/main_tapwing.dart` (`--dry-run` previews changes) |
| `tapwing check <paths>` | Validate flows offline with line/column errors and suggestions (`--dump-ir` prints JSON) |
| `tapwing test <paths>` | Run flow files or workspace directories |
| `tapwing hierarchy` | Dump the live UI tree in Maestro attributes |

Common `tapwing test` flags (`tapwing test --help` lists them all):

| Flag | Purpose |
|---|---|
| `--device <id>`, `-p android\|ios` | Pick a device (adb serial, simulator name, or UDID) or force the platform ([devices](docs/devices.md)) |
| `--include-tags`, `--exclude-tags` | Filter flows by tag |
| `--shard-split <N>`, `--shard-all <N>` | Run in parallel across `N` devices |
| `--format junit\|html\|html-detailed` | Generate a CI report to `--output` ([reports](docs/workspaces-and-reports.md)) |
| `--trace` | Write an offline step-by-step trace viewer ([diagnostics](docs/failure-diagnostics.md)) |
| `--app-errors report\|fail\|off` | How to handle overflows, exceptions, and crashes; default `report` ([app health](docs/app-health.md)) |
| `--input auto\|flutter\|native` | Choose which driver controls the UI; default `auto` |
| `-e KEY=VALUE` | Pass an environment variable to the flow |

Exit codes: `0` all flows passed, `1` a flow failed, `2` CLI, config, device, or report error.

## Documentation

| Guide | Covers |
|---|---|
| [Set up your app](docs/setup.md) | Flavors, pub workspaces, custom entrypoints, local driver checkouts |
| [Devices](docs/devices.md) | Device selection, iOS Simulator setup, connection troubleshooting |
| [Workspaces, sharding and reports](docs/workspaces-and-reports.md) | `config.yaml`, tags, execution order, sharding, JUnit and HTML reports |
| [App health](docs/app-health.md) | Catching layout overflows, uncaught exceptions, and crashes |
| [Failure diagnostics and trace viewer](docs/failure-diagnostics.md) | Debugging failing flows and reading offline traces |
| [Native lanes](docs/native-lanes.md) | System dialogs, WebViews, and apps without `tapwing_driver` |
| [Supported commands](docs/commands.md) | Maestro commands, selectors, timeouts, and platform differences |
| [Maestro compatibility](docs/compatibility.md) | Parser parity, JS engine, intentional differences, test coverage |
| [Install](docs/install.md) | Homebrew, `install.sh` flags, building from source, driver lookup, releasing |
| [Architecture and roadmap](PLAN.md) | Design decisions and project roadmap |
| [Contributing](CONTRIBUTING.md) | Repository layout, building from source, running tests and benchmarks |

## Limitations

- Targets Android (emulators and physical devices) and the iOS Simulator. Physical iOS devices are not supported.
- Flutter apps use the fast in-app driver; other apps run black-box through the native lane.
- AI commands (`assertWithAI`, `assertNoDefectsWithAI`, `extractTextWithAI`) parse cleanly but fail at runtime because they require Maestro Cloud.
- Pre-1.0 (`0.x`): commands, flags, and output formats may still change between minor releases.

## License

[Apache License 2.0](LICENSE). Tapwing's native drivers (`drivers/android`, `drivers/ios`) include code ported from Maestro under Apache-2.0. See `drivers/*/THIRD_PARTY_NOTICES.md` (and `share/tapwing/licenses/THIRD_PARTY_NOTICES.md` in release tarballs) for attribution.

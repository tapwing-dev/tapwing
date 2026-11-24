# Set up your Flutter app

`tapwing init` connects your Flutter app to Tapwing without touching your production code. It adds `tapwing_driver` under `dev_dependencies` in `pubspec.yaml` (keeping your comments and formatting intact) and creates a separate `lib/main_tapwing.dart` entrypoint.

## Basic setup

Run `tapwing init` inside your Flutter project:

```sh
cd path/to/your_flutter_app
tapwing init --pub-get
```

Build and install the app with the generated entrypoint:

```sh
# Android (profile build)
flutter build apk --profile -t lib/main_tapwing.dart
adb install -r build/app/outputs/flutter-apk/app-profile.apk

# iOS Simulator (debug build; the simulator has no profile mode)
flutter build ios --simulator --debug -t lib/main_tapwing.dart
xcrun simctl install booted build/ios/iphonesimulator/Runner.app
```

Use a profile or debug build. In a release build the driver stays off (the app then behaves as if it had no `tapwing_driver` and runs through the [native lane](native-lanes.md)), so that an app shipped with the Tapwing entrypoint by mistake cannot be driven by another app on the device. To test a release build on purpose, add `--dart-define=TAPWING_ALLOW_RELEASE=true`.

Then run your Maestro flows:

```sh
tapwing test .maestro
```

## Common setups

**Flavors or custom entrypoints.** Point `--target` at your flavor entrypoint and give the generated file its own name:

```sh
tapwing init --target lib/main_dev.dart --output lib/main_tapwing_dev.dart --pub-get
```

The generated entrypoint handles both `async` `main()` functions and `List<String> args` automatically.

**Pub workspaces (monorepos).** If your workspace contains more than one Flutter app, tell Tapwing which one to configure:

```sh
tapwing init --app apps/my_app --pub-get
```

**Local checkout of Tapwing.** Developing against a local clone instead of git? Use `--driver-path`:

```sh
tapwing init --driver-path ../tapwing/packages/tapwing_driver --pub-get
```

**Preview before writing.** Pass `--dry-run` to see the exact diff without changing any files. Re-running `tapwing init` is safe: it skips files that are already set up unless you pass `--force`.

## Flags

| Flag | What it does |
|---|---|
| `[PROJECT_DIR]` | Project or workspace directory (default: `.`) |
| `--pub-get` | Run `flutter pub get` after editing `pubspec.yaml` |
| `--dry-run` | Print the diff without writing files |
| `--force` | Overwrite an existing `main_tapwing.dart` or `tapwing_driver` entry |
| `--app <DIR>` | App directory inside a multi-app pub workspace |
| `--target <FILE>` | App entrypoint to wrap (default: `lib/main.dart`) |
| `--output <FILE>` | Path for the generated entrypoint (default: `lib/main_tapwing.dart`) |
| `--driver-path <DIR>` | Use a local `path:` dependency instead of git |
| `--driver-ref <REF>` | Override the git ref (default: `v<tapwing version>`) |
| `--driver-url <URL>` | Override the git repository URL |

By default, `tapwing init` pins `tapwing_driver` to the git tag matching your installed `tapwing` version so the CLI and driver stay in sync.

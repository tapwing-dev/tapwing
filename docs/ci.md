# Continuous Integration

Run Tapwing in CI using standard GitHub Actions runners (or any CI worker with Android emulators or macOS iOS Simulators). You do not need Java or Maestro installed on the runner.

## Running Your App Flows on Android (GitHub Actions)

```yaml
name: E2E (Android)
on: [pull_request]

jobs:
  android-e2e:
    runs-on: ubuntu-latest # or macos-latest for KVM/HVF acceleration
    steps:
      - uses: actions/checkout@v4

      - uses: subosito/flutter-action@v2
        with:
          channel: stable

      - name: Build Tapwing entrypoint APK (profile)
        run: flutter build apk --profile -t lib/main_tapwing.dart

      - name: Install Tapwing CLI + bundled native drivers
        run: |
          curl -fsSL https://raw.githubusercontent.com/tapwing-dev/tapwing/main/scripts/install.sh | bash
          echo "$HOME/.local/bin" >> "$GITHUB_PATH"

      - name: Validate flows before booting emulator
        run: tapwing check .maestro

      - name: Run flows on Android emulator
        uses: reactivecircus/android-emulator-runner@v2
        with:
          api-level: 35
          arch: x86_64
          profile: pixel_6
          script: |
            adb install -r build/app/outputs/flutter-apk/app-profile.apk
            tapwing test .maestro \
              --format junit \
              --output build/tapwing-junit.xml \
              --test-output-dir build/tapwing-artifacts \
              --trace

      - name: Upload failure & trace artifacts
        if: always()
        uses: actions/upload-artifact@v4
        with:
          name: tapwing-android-artifacts
          path: |
            build/tapwing-junit.xml
            build/tapwing-artifacts/
```

Tips:
- Pass `--app-errors fail` once your suite has resolved existing layout overflows and uncaught errors (see [App health](app-health.md)).
- On multi-emulator runners, pass `--shard-split 2` to split workspace flows across devices in parallel ([Workspaces, sharding and reports](workspaces-and-reports.md)).
- Always upload `<test-output-dir>/` with `if: always()`: on failures Tapwing saves annotated screenshots, pruned UI hierarchies, and `trace.html` (offline viewer with redacted HTTP logs).

## Running on iOS Simulator (macOS Runners)

```yaml
      - name: Build Tapwing entrypoint for Simulator (debug)
        run: flutter build ios --simulator --debug -t lib/main_tapwing.dart

      - name: Boot Simulator & run Tapwing
        run: |
          UDID=$(xcrun simctl list devices available | sed -n 's/.*iPhone.*\(([0-9A-F-]\{36\})\).*/\1/p' | head -n1)
          xcrun simctl boot "$UDID"
          xcrun simctl install "$UDID" build/ios/iphonesimulator/Runner.app
          tapwing test .maestro --device "$UDID" --format junit --output build/ios-junit.xml --trace
```

The iOS Simulator has no Flutter profile mode, so build `--simulator --debug -t lib/main_tapwing.dart`.

## Tapwing's Own Repository Workflows

For contributors modifying Tapwing itself, CI lives under [`.github/workflows/`](../.github/workflows/ci.yml) (`rust`, `dart`, `android-driver`, `scripts`, gated by `CI Gate`). See [CONTRIBUTING.md](../CONTRIBUTING.md#continuous-integration) and [install.md](install.md#releasing) for full details.

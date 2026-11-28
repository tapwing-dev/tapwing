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

### Android emulator end-to-end run

[`ci-android-e2e.yml`](../.github/workflows/ci-android-e2e.yml) is the only workflow that runs the CLI, the in-app Dart driver and the native driver APK together on a device. It boots an API 35 x86_64 emulator (`google_apis`, `pixel_6_pro` profile, KVM) on `ubuntu-latest` and calls [`scripts/ci-android-e2e.sh`](../scripts/ci-android-e2e.sh), which works the same on a developer machine:

```sh
scripts/ci-android-e2e.sh --serial emulator-5554
```

The script builds the release `tapwing` binary, the driver APK and the sample app profile APK (`-t lib/main_tapwing.dart`, for the device's ABI), installs the app, and then calls `tapwing test` with `--trace`, a JUnit and a JSON report for each of these runs:

| Run | Flows | Must exit with |
|---|---|---|
| `positive` | Every top-level flow of `examples/sample_app/.maestro`, in one call | `0` |
| `night/<flow>` | `f2_dark_mode`, after the script switched the device to night mode | `0` |
| `negative/<flow>` | Each flow of `.maestro/negative/`, one call per flow | `1` (`2` is a runner or device error and fails the job) |

`f2_dark_mode` has its own run because the app follows the system theme and the flow does not switch it, so it cannot share a run with the flows that assert the light theme. The script passes flow files, not the directory, so a `config.yaml` cannot widen the selection. `ios/` (iOS Simulator) and `web/` (needs a web server on the host) do not run here. A flow that cannot pass on the CI emulator goes into `EXCLUDED_FLOWS` at the top of the script with its reason, and the log and the summary print every entry. The list is empty today.

The script sets what the flows depend on and restores it on exit: the display density (560 dpi, the density the reference screenshots were recorded at) and night mode. It fails before the first flow when the system locale is not English, because `f5_permission_dialog` taps the English labels of the system permission dialog.

Every build, every `tapwing test` call and every workflow step has a time limit. A build or run that exceeds its limit fails the job with `TIMEOUT: <step>`.

The workflow runs on every push to `main`, on pull requests that touch `crates/`, `packages/`, `drivers/android/`, `examples/sample_app/`, the script or the workflow, and on demand (`workflow_dispatch`). It uploads `build/android-e2e/` as the `android-e2e-<run id>-<attempt>` artifact after a passing and after a failing run: per run `trace.html`, `trace.json`, `junit.xml`, `report.json` and failure screenshots, plus `summary.md`, `device.txt` and `logcat.txt`.

**Not required yet.** The job is not in the `CI Gate` needs list. It becomes required after a week of stable runs on `main` ([issue #3](https://github.com/tapwing-dev/tapwing/issues/3)). A required check must report on every pull request and this workflow skips pull requests outside its paths, so requiring it means calling it from `ci.yml` behind a changed-paths job and letting `CI Gate` accept a skipped result for it.

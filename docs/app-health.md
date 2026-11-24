# App health

Standard UI tests only catch what you explicitly assert. A screen can have a yellow-and-black layout overflow, fail to load an image, or throw an uncaught exception, and a flow will still go green.

Tapwing watches what your Flutter app reports while a flow runs and attaches every issue to the step that triggered it, without changing your YAML flows.

```sh
# Report issues under each step (default; does not fail the flow)
tapwing test .maestro/

# Fail on the first step that triggers an issue
tapwing test .maestro/ --app-errors fail

# Ignore a known issue and screenshot the rest
tapwing test .maestro/ \
  --app-errors fail \
  --app-errors-ignore 'overflowed by .* on the bottom' \
  --app-errors-screenshot
```

In the terminal, issues show up right under the step that caused them:

```
  ✓    63.9 ms  tapOn "Show overflow"
      ⚠ layoutOverflow: A RenderFlex overflowed by 62 pixels on the right.
  ✓   117.2 ms  tapOn "Load broken image"
      ⚠ imageLoadError: SocketException: Connection refused (OS Error: Connection refused, errno = 111), …
```

## What it catches

| Kind | Where it comes from | Builds |
|---|---|---|
| `layoutOverflow` | `FlutterError.onError` in debug; layout check after each settle in profile/release | All |
| `buildError`, `layoutError`, `paintError`, `imageLoadError` | `FlutterError.onError` | All |
| `frameworkAssertion` | Other Flutter errors (such as `setState() called after dispose()`) | Debug |
| `uncaughtError` | `PlatformDispatcher.onError` | All |
| `appCrash` | App process exited mid-run (`logcat -b crash` on Android, `.ips` crash report on iOS Simulator) | All |

## Configuration

| Flag | Purpose |
|---|---|
| `--app-errors report\|fail\|off` | `report` (default) prints and records issues without failing the flow; `fail` fails the flow; `off` disables observation |
| `--app-errors-ignore <regex>` | Ignore matching issues when deciding pass/fail (repeatable; ignored issues are still logged) |
| `--app-errors-screenshot` | Save a screenshot of the first occurrence of each issue under `<test-output-dir>/app-issues/` |

You can set workspace defaults in `config.yaml` under `tapwing:` (Maestro warns about the unknown key and ignores it):

```yaml
tapwing:
  appErrors: fail
  appErrorsIgnore:
    - "overflowed by .* on the bottom"
  appErrorsScreenshot: true
```

CLI flags override `config.yaml`; `--app-errors-ignore` patterns from the CLI are added to the ones in `config.yaml`.

## Crashes

If the app crashes during a step, Tapwing checks whether the process is still alive once a second instead of waiting for a generic socket timeout. You get the crash reason within 1–2 seconds (for example `App crashed during 'tapOn "…"'` with the Android crash log or iOS `.ips` excerpt) rather than a vague `connection lost` message. A live but frozen app still times out after 10 seconds of silence.

## Reports and step attribution

Each issue is tagged with the step that triggered it. If an error fires asynchronously right after a step finishes, Tapwing attaches it back to that step (`(after …)` in the console). Any remaining buffered issues at the end of a flow go to the last step.

- **JSON (`--report`):** steps include `appIssues` (`kind`, `message`, `summary`, `stack`, `count`, `screenshot`), and each flow has an `appIssueSummary` (`total`, `ignored`, `byKind`).
- **JUnit (`--format junit`):** issues are written to `<system-err>`. Clean flows keep the exact same XML as Maestro.
- **HTML (`--format html` / `html-detailed`):** flows with issues show a `⚠ N app issues` badge, stay expanded even when passing, and list issues inline per step.

## No side effects

The hooks only observe and always forward to your app's existing error handlers, so Sentry, Crashlytics, and Flutter's red error screen keep working as usual. Passing `--app-errors off` disables the hooks completely.

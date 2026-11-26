# Maestro compatibility

Tapwing is designed to run existing Maestro flows without edits. Here is what matches Maestro, where Tapwing intentionally differs, and how compatibility is verified.

## Parser (`tapwing check`)

`tapwing check` parses your flows offline using the same rules as Maestro and points to errors with line, column, and "Did you mean" hints:

```sh
tapwing check .maestro
tapwing check .maestro/login.yaml --dump-ir   # print Maestro-format JSON IR
```

A directory is walked recursively (each directory once, so symlinks that lead back into the tree are safe). `check` exits non-zero when a flow is invalid and also when a directory you passed contains no flow files, so a wrong path cannot pass in CI.

With `--dump-ir`, stdout carries exactly one JSON document and all status lines go to stderr, so the output can be piped into `jq`. A single flow file prints one `{ "path", "commands" }` object; a directory or several paths print an array of those objects, with invalid flows left out (their errors are on stderr).

Every Maestro command, shorthand syntax, `runFlow`, `retry`, `repeat`, `when`/`while`, `runScript`, `onFlowStart`/`onFlowComplete`, `env`, `label`, and `optional` is supported. Parsing 544 flows takes ~33 ms (compared to ~2.6 s for a single file in Maestro due to JVM startup).

## Flow execution and JavaScript

Tapwing runs the same orchestration rules as Maestro:

- **Control flow:** file-based and inline `runFlow`, `repeat` (`times` and `while`), `retry` (only on command errors, not script or transport errors), `onFlowStart`, and `onFlowComplete` (always runs, even if the flow fails).
- **Variables:** `-e KEY=VALUE` overrides top-level flow `env` and shell variables. Shell environment variables prefixed with `MAESTRO_` are injected automatically (preserving the `MAESTRO_` prefix; empty values and internal shard variables `MAESTRO_SHARD_ID`/`MAESTRO_SHARD_INDEX` are excluded), alongside built-in variables `MAESTRO_FILENAME`, `MAESTRO_DEVICE_UDID`, `MAESTRO_SHARD_INDEX`, and `MAESTRO_SHARD_ID`.
- **JavaScript:** powered by an embedded QuickJS engine supporting `output`, `maestro` (`platform`, `copiedText`), `json()`, `relativePoint()`, `console.log`, synchronous `http.{get,post,put,delete,request}` (with `headers`, `body`, and `multipartForm`), and `faker` (including `faker.expression(...)`). Like Maestro, scripts have no time limit (`http.*` keeps Maestro's 5-minute timeout), but the first Ctrl-C or `SIGTERM` stops a running script or an in-flight `http.*` request right away and device cleanup still runs. A script that allocates more than 1 GiB fails with `JavaScript ran out of memory` instead of taking the whole run down.

## CLI flags parity

Tapwing mirrors Maestro 2.12.1's global and `test` flags so existing CI invocations work when switching `maestro` to `tapwing`. Maestro flags that Tapwing does not implement are accepted as hidden options and emit a warning on `stderr` naming the flag.

### Maestro 2.12.1 global and `test` flags

| Flag | Status | Behavior in Tapwing |
|---|---|---|
| `-v`, `-V`, `--version`, `-h`, `--help` | Supported | Prints Tapwing + bundled driver version or help |
| `-p`, `--platform <android\|ios>` | Supported | Forces target platform |
| `--device`, `--udid <ID[,ID...]>` | Supported | Selects device(s) or shard targets |
| `--port <PORT>` | Supported | Host TCP port the Flutter driver dials (`0` = ephemeral) |
| `--config`, `--include-tags`, `--exclude-tags` | Supported | Workspace config and tag filtering |
| `--shard-split`, `--shard-all`, `-s`, `--shards` | Supported | Multi-device sharded execution |
| `-e`, `--env <KEY=VALUE>` | Supported | Sets flow env vars (overrides shell `MAESTRO_*` and flow `env:`) |
| `--format`, `--output`, `--test-suite-name` | Supported | `junit`, `html`, `html-detailed`, or `noop` suite reports |
| `--test-output-dir`, `--debug-output`, `--flatten-debug-output` | Supported | Controls run artifact directory layout |
| `--no-ansi`, `--ansi`, `--no-color`, `--color` | Accepted (warns) | Ignored no-op (Tapwing emits plain text output) |
| `--verbose` | Accepted (warns) | Ignored no-op |
| `--host`, `--driver-host-port` | Accepted (warns) | Ignored no-op (Tapwing manages local driver ports) |
| `-c`, `--continuous` | Accepted (warns) | Ignored no-op (watch mode is not implemented; runs once) |
| `--headless`, `--screen-size` | Accepted (warns) | Ignored no-op (Web target is not supported) |
| `--analyze`, `--api-url`, `--api-key` | Accepted (warns) | Ignored no-op (Maestro Cloud / AI insights are not supported) |
| `--reinstall-driver`, `--no-reinstall-driver` | Accepted (warns) | Ignored no-op (Tapwing manages driver installation automatically) |
| `--apple-team-id` | Accepted (warns) | Ignored no-op (Tapwing does not sign Xcode runner bundles) |

### Tapwing-specific `test` flags

| Flag | Purpose |
|---|---|
| `--report <PATH>` | Write JSON report with per-command timings and lane stats |
| `--input <auto\|flutter\|native>` | Control UI routing lane (default `auto`) |
| `--settle-policy <v2\|legacy>`, `--check-settle` | Driver idle detection policy and post-action settle check |
| `--strict-input` | Fail `inputText`/`eraseText` when no text field is focused |
| `--maestro-compat <default\|strict>` | Emulate Maestro behavior Tapwing changes on purpose in `strict` mode (iOS `hideKeyboard` failure, iOS Simulator `clearState` by reinstalling the app) |
| `--app-errors`, `--app-errors-ignore`, `--app-errors-screenshot` | Capture/fail on Flutter overflows, exceptions, and crashes |
| `--trace`, `--trace-screenshots <failures\|steps>` | Write offline `trace.html` + `trace.json` diagnostics |
| `--native-driver <DRIVER>` | Deprecated no-op (warns for non-`tapwing` values) |

## Intentional differences

A few places where Tapwing goes slightly beyond Maestro by default:

- **App health reporting.** Tapwing observes layout overflows, uncaught exceptions, and crashes during runs (`--app-errors report` by default). It never fails a flow unless you opt in with `--app-errors fail`, and `--app-errors off` disables it completely. See [App health](app-health.md).
- **iOS `hideKeyboard`.** Tapwing dismisses the keyboard with a warning even if a Flutter text field remains focused. Pass `--maestro-compat strict` to reproduce Maestro's iOS failure behavior.
- **iOS Simulator `clearState`.** Maestro terminates the app, uninstalls it and installs it again, and the new data container has to be looked up again before the launch. Tapwing terminates the app and empties its data container on the host instead, and the container keeps its path. On an iPhone 17 simulator (iOS 27.2) with the 139 MB debug sample app this cut the `launchApp` clear phase from a median of about 720 ms to about 490 ms and the launch phase from about 270 ms to about 140 ms. A wipe deletes everything in the container except the container manager's metadata file, recreates `Documents`, `Library/Caches`, `Library/Preferences`, `SystemData` and `tmp`, drops the app's `NSUserDefaults` from the simulator's preferences daemon (which would otherwise serve a value written just before the app stopped from memory, even with the plist file gone), and runs `simctl privacy reset all` unless the flow's `permissions` have an `all` entry (the launch then sets every service anyway). It only deletes a path of the form `<simulator>/data/Containers/Data/Application/<UUID>` that holds the metadata file and names the app. For any other path, or when a step fails, it prints a warning on stderr and reinstalls the app like Maestro. On an iOS 27.2 simulator, preferences, files, caches and `simctl privacy` services (location checked) are cleared as with a reinstall; these differ:
  - The **notification permission** (`UNUserNotificationCenter` authorization) survives a wipe, while a reinstall resets it. `simctl privacy` cannot reset it either.
  - **Keychain items** and **app group containers** are not in the data container, so they survive in both modes (`clearKeychain: true` clears the keychain).
  - Not checked: preference domains other than the app's own bundle id (a `UserDefaults(suiteName:)` suite) and app extension containers.

  Pass `--maestro-compat strict` to reinstall exactly like Maestro. Android is unchanged. `examples/sample_app/.maestro/ios/f9_clear_state.yaml` shows a preference and a file gone after `clearState`.
- **AI commands.** `assertWithAI`, `assertNoDefectsWithAI`, and `extractTextWithAI` parse cleanly but fail at runtime with a clear message, since they rely on Maestro Cloud.
- **Workspace `tapwing:` config.** `config.yaml` supports an optional `tapwing:` block for defaults like `appErrors`. Maestro warns about the unknown key and ignores it.

## How compatibility is verified

- **Parser:** checked against **463/463** fixtures from Maestro's repository plus **81/81** custom edge-case probes ([compat/README.md](../compat/README.md)).
- **Workspace planner:** **42/42** test cases compared directly against Maestro's planner (globs, tags, ordering, `continueOnFailure`).
- **Orchestrator:** **160** Maestro integration tests ported to run against an in-memory driver, covering selectors, control flow, scripting, timeouts, and error semantics.

See [CONTRIBUTING.md](../CONTRIBUTING.md) for how to run these test suites locally.

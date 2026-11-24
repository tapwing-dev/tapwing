# Maestro compatibility

Tapwing is designed to run existing Maestro flows without edits. Here is what matches Maestro, where Tapwing intentionally differs, and how compatibility is verified.

## Parser (`tapwing check`)

`tapwing check` parses your flows offline using the same rules as Maestro and points to errors with line, column, and "Did you mean" hints:

```sh
tapwing check .maestro
tapwing check .maestro/login.yaml --dump-ir   # print Maestro-format JSON IR
```

Every Maestro command, shorthand syntax, `runFlow`, `retry`, `repeat`, `when`/`while`, `runScript`, `onFlowStart`/`onFlowComplete`, `env`, `label`, and `optional` is supported. Parsing 544 flows takes ~33 ms (compared to ~2.6 s for a single file in Maestro due to JVM startup).

## Flow execution and JavaScript

Tapwing runs the same orchestration rules as Maestro:

- **Control flow:** file-based and inline `runFlow`, `repeat` (`times` and `while`), `retry` (only on command errors, not script or transport errors), `onFlowStart`, and `onFlowComplete` (always runs, even if the flow fails).
- **Variables:** `-e KEY=VALUE` overrides top-level flow `env`, alongside built-in variables like `MAESTRO_FILENAME`, `MAESTRO_DEVICE_UDID`, `MAESTRO_SHARD_INDEX`, and `MAESTRO_SHARD_ID`.
- **JavaScript:** powered by an embedded QuickJS engine supporting `output`, `maestro` (`platform`, `copiedText`), `json()`, `relativePoint()`, `console.log`, synchronous `http.{get,post,put,delete,request}` (with `headers`, `body`, and `multipartForm`), and `faker` (including `faker.expression(...)`).

## Intentional differences

A few places where Tapwing goes slightly beyond Maestro by default:

- **App health reporting.** Tapwing observes layout overflows, uncaught exceptions, and crashes during runs (`--app-errors report` by default). It never fails a flow unless you opt in with `--app-errors fail`, and `--app-errors off` disables it completely. See [App health](app-health.md).
- **iOS `hideKeyboard`.** Tapwing dismisses the keyboard with a warning even if a Flutter text field remains focused. Pass `--maestro-compat strict` to reproduce Maestro's iOS failure behavior.
- **AI commands.** `assertWithAI`, `assertNoDefectsWithAI`, and `extractTextWithAI` parse cleanly but fail at runtime with a clear message, since they rely on Maestro Cloud.
- **Workspace `tapwing:` config.** `config.yaml` supports an optional `tapwing:` block for defaults like `appErrors`. Maestro warns about the unknown key and ignores it.

## How compatibility is verified

- **Parser:** checked against **463/463** fixtures from Maestro's repository plus **81/81** custom edge-case probes ([compat/README.md](../compat/README.md)).
- **Workspace planner:** **42/42** test cases compared directly against Maestro's planner (globs, tags, ordering, `continueOnFailure`).
- **Orchestrator:** **160** Maestro integration tests ported to run against an in-memory driver, covering selectors, control flow, scripting, timeouts, and error semantics.

See [CONTRIBUTING.md](../CONTRIBUTING.md) for how to run these test suites locally.

# Workspaces, sharding and reports

`tapwing test` accepts individual flow files or whole workspace directories and follows the same planning rules as Maestro. Replacing `maestro test .maestro/` with `tapwing test .maestro/` works out of the box.

## Workspaces and `config.yaml`

When you pass a directory, Tapwing runs the top-level flow files in it. Files inside subfolders are treated as sub-flows and only run when invoked via `runFlow`.

If the directory contains a `config.yaml` (or `config.yml`), or if you pass `--config <file>`, Tapwing applies its settings:

- **`flows` globs** pick which files to include or exclude (`*` stays in one folder, `**` crosses folders, `!pattern` excludes).
- **`executionOrder.flowsOrder`** runs the listed flows first, in that order. With `continueOnFailure: false`, a failure stops the rest of the ordered list while remaining unordered flows still run.
- **`platform.android.disableAnimations: true`** sets Android's three system animation scales to `0` for the run and restores their previous values when finished.

Config files and other non-flow YAML files are skipped automatically. Before touching any device, Tapwing validates every selected flow and CLI flag, exiting immediately with code `2` if anything is wrong.

## Filtering by tags

Use `--include-tags` and `--exclude-tags` (comma-separated) to filter a workspace run:

```sh
tapwing test .maestro --include-tags smoke,checkout
tapwing test .maestro --exclude-tags slow
```

CLI tags are merged with `includeTags` and `excludeTags` in `config.yaml`. Tag filters are ignored when you pass a single flow file directly.

## Running in parallel (sharding)

Spread flows across multiple connected devices or run the full suite on all of them at once:

```sh
# Split flows round-robin across 2 devices
tapwing test .maestro --shard-split 2

# Run every flow on 2 specific devices
tapwing test .maestro --shard-all 2 --device "emulator-5554,emulator-5556"
```

- `--shard-split <N>` distributes flows evenly across `N` devices (if `N` exceeds the number of flows, Tapwing clamps it with a warning). The older `-s` / `--shards` flag is kept as a deprecated alias.
- `--shard-all <N>` runs the entire suite on each of the `N` devices.

Each shard uses its own ephemeral host port, prefixes console output with `[shard N] `, and exposes `MAESTRO_SHARD_INDEX` (0-based), `MAESTRO_SHARD_ID` (1-based), and `MAESTRO_DEVICE_UDID` to flows.

## Reports and output directories

```sh
tapwing test .maestro --format junit --output report.xml --test-suite-name "Smoke"
tapwing test .maestro --format html-detailed --output report.html
tapwing test .maestro --report timings.json
```

| Flag | Purpose |
|---|---|
| `--format <FMT>` | `junit`, `html`, `html-detailed`, or `noop` (default) |
| `--output <PATH>` | Report file path (default: `report.xml` for JUnit, `report.html` for HTML) |
| `--test-suite-name <NAME>` | Custom suite name for JUnit XML and the HTML header |
| `--report <PATH>` | Per-command JSON timing report (includes per-shard breakdown on sharded runs) |
| `--test-output-dir <DIR>` | Directory for screenshots, diffs, failures, and traces (also configurable via `testOutputDir` in `config.yaml`) |
| `--debug-output <DIR>` | Base directory for run artifacts when `--test-output-dir` is unset |
| `--flatten-debug-output` | Write artifacts directly into the output root without `run-*` or `shard-N/` subfolders |

JUnit reports follow Maestro's XML structure (one `<testsuite>` per shard, custom `junitId` / `junitClassname` properties, and `[shard N] ` prefixes on failures). HTML and `html-detailed` reports are single self-contained files with no external CDN requests.

## Exit codes

| Code | Meaning |
|---|---|
| `0` | All flows passed |
| `1` | One or more flows failed |
| `2` | Invalid CLI flags, broken flow/config, device error, or report write error |

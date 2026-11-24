# Maestro Parser Compatibility

Tapwing parses YAML flow files into the exact same IR as Maestro 2.12.1 (`MaestroCommand` et al.). This directory proves parity.

| Path | Description |
|---|---|
| `maestro-fixtures/` | 463 YAML files from the Maestro repository (`maestro-test`, `maestro-orchestra`, `maestro-cli`, and e2e flows). Apache 2.0 license, see [NOTICE](maestro-fixtures/NOTICE.md) |
| `probes/` | 81 custom edge-case tests covering Jackson YAML scalar typing, all command forms, and error states |
| `maestro-ir/`, `probes-ir/` | Reference Maestro output for each file: `{"commands": [...]}` or `{"error": ...}` |
| `harness/MaestroIrDump.kt` | Small Kotlin program calling `YamlCommandReader.readCommands` from the installed Maestro JARs and serializing the result with Jackson |
| `dump_maestro_ir.sh` | Regenerates `maestro-ir/` and `probes-ir/` (requires `maestro`, `kotlinc`, `java`) |
| `workspace-cases.json` | Workspace planning cases (`tapwing test <dirs>`): Maestro's `WorkspaceExecutionPlannerTest` and `…ErrorsTest` inputs over `maestro-fixtures/maestro-orchestra/workspaces`, plus Tapwing extras |
| `workspace-plans.json`, `harness/WorkspacePlanDump.kt`, `dump_workspace_plans.sh` | What Maestro 2.12.1's `WorkspaceExecutionPlanner` decides for each case (flows, `executionOrder` sequence, or the error), regenerated from the installed Maestro JARs |

Test command: `cd crates/tapwing && cargo test --test compat`. For every file it verifies:
- **Parsed flow:** IR matches field-for-field;
- **Rejected flow:** Tapwing rejects it as well, with the same error title ("Unknown Property: x", "Invalid Command: y", "Media File Not Found"…).

Per-file reports are written to `crates/tapwing/target/compat-*.txt`.

Status: **463/463** fixtures and **81/81** probes passing.

Workspace planning: `cargo test --test workspace` plans every case in `workspace-cases.json` and requires the same flows, the same sequence (in order) and `continueOnFailure`, the identical message for Maestro's validation errors, and the same title and file for flow syntax errors. Status: **42/42** cases identical.

## Jackson Quirks Replicated (Verified via `probes/`)

- **Booleans:** `yes/no/on/off/True/...` parse as booleans and `optional: 1` means `true`. `optional: null` falls back to the default value.
- **String fields:** receive the raw scalar text, so `timeout: 010`, `1.50`, and `0x10` remain verbatim. Inside selectors, `swipe`, and other custom deserializers, however, Maestro reads the tree first, producing canonical numbers (`1.50` → `1.5`).
- **Enums:** case-insensitive (`direction: up`, `platform: iOS`). For `swipe`, only ALL CAPS or all lowercase are accepted.
- **Duplicate keys:** last value wins. Merge key `<<` fails with "Unknown Property". Alias `*a` is read as literal text `a` because Jackson does not resolve aliases in this mode.
- **Map form of `inputText`/`evalScript`:** ignores `optional`.
- **`setOrientation`:** treats quoted `optional: "true"` as `false`, whereas `swipe` treats it as `true`.
- **`onFlowStart`:** accepts a list only, not `{commands: [...]}`.
- **Error precedence:** syntax of the entire command list (including hooks and nested `commands:`) is checked before resolving files for `runFlow`/`runScript`/`addMedia`.
- **`env`:** prepends a `defineVariablesCommand`. For inline `runFlow`/`retry`, each command receives the prefix individually.

Error locations (`line:col`) use Tapwing's own tighter spans. Maestro (Jackson) typically points past the offending token, so exact positions are not compared between the two tools.

# Migrating from Maestro

Tapwing runs existing Maestro 2.12.1 YAML flows, workspaces, and JavaScript helpers without rewriting your tests. Use this checklist to switch a project over safely.

## 1. Install and Wire the App Driver

1. Install Tapwing ([install.md](install.md)); Maestro and a JDK are no longer required.
2. From your Flutter project root, add `tapwing_driver` (`dev_dependency`) and create `lib/main_tapwing.dart`:
   ```sh
   tapwing init --pub-get
   ```
   Production `lib/main.dart` is left untouched (see [setup.md](setup.md) for flavors, monorepos, or custom zone wrappers).
3. Build with `-t lib/main_tapwing.dart`:
   ```sh
   # Android (profile)
   flutter build apk --profile -t lib/main_tapwing.dart

   # iOS Simulator (debug)
   flutter build ios --simulator --debug -t lib/main_tapwing.dart
   ```
   *(If you run a build without `tapwing_driver`, Tapwing automatically falls back to its [native lanes](native-lanes.md).)*

## 2. Validate Existing Flows

```sh
tapwing check .maestro
```

`tapwing check` parses every `.yaml` / `.yml` flow using Maestro 2.12.1 rules, flags unknown keys or syntax issues with "Did you mean" hints, checks sub-flow cycles, and exits non-zero if no flows match.

## 3. Replace `maestro test` Commands

```sh
# Before
maestro test .maestro/ --include-tags smoke --format junit --output report.xml

# After
tapwing test .maestro/ --include-tags smoke --format junit --output report.xml
```

Existing Maestro CLI flags (`-p`, `--device`/`--udid`, `--config`, `--include-tags`, `--exclude-tags`, `--shard-split`, `--shard-all`, `-e`, `--format`, `--output`, `--test-suite-name`, `--test-output-dir`, `--debug-output`, `--flatten-debug-output`) keep the same meaning. Unused Maestro flags (`--no-ansi`, `--host`, `--driver-host-port`, `--headless`, `--reinstall-driver`, `--apple-team-id`, etc.) are accepted with a deprecation/no-op warning so existing CI scripts do not break immediately.

See [compatibility.md](compatibility.md) for the complete flag matrix.

## 4. Review Intentional Differences

When moving green Maestro suites onto Tapwing, note four intentional differences:

| Area | Maestro Behavior | Tapwing Behavior |
|---|---|---|
| **App health errors** | Ignored unless an assertion fails | Observed and printed under each step (`--app-errors report` default); flows stay green unless you pass `--app-errors fail` ([app-health.md](app-health.md)) |
| **iOS `hideKeyboard`** | Fails if an editable Flutter field keeps focus | Hides keyboard and emits a warning instead of failing; pass `--maestro-compat strict` to match Maestro's error |
| **Maestro Cloud AI commands** | Calls Maestro Cloud | `assertWithAI`, `assertNoDefectsWithAI`, `extractTextWithAI` parse cleanly and fail at runtime (`--analyze`/`--api-key` warn and no-op) |
| **Workspace `tapwing:` block** | Ignored with an unknown-key warning | Optional `tapwing:` block in `config.yaml` sets defaults (`appErrors`, `appErrorsIgnore`, `appErrorsScreenshot`, `settleLoadTimeoutMs`, `settleHttpIgnore`) |

## 5. Run a Dual-Mode Check Before Turning Off Maestro

If you want to compare Tapwing's in-app semantics against pure black-box UI behavior:

```sh
tapwing test .maestro --input flutter   # verify fast in-app path only
tapwing test .maestro --input native    # force native UiAutomator / XCUITest lane
tapwing test .maestro                   # recommended: auto hybrid routing
```

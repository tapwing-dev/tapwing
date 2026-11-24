---
name: readme-and-docs-curator
description: >-
  Maintains concise, accurate, drift-free English documentation across
  `README.md`, `PLAN.md`, and package/bench READMEs. Use when updating docs
  after a milestone, auditing `README.md` for clarity, or verifying that
  documented commands, flags, file links, and test counts match the codebase.
---

# README & Documentation Curator

Keep Tapwing's documentation crisp, scannable, and 100% synchronized with the actual codebase.

## 1. Document Ownership (Zero Duplication)

- **`README.md` (Root):** a short entry point (about 150 lines) in this order: one-sentence pitch, why Tapwing, a flow example, how it works, install, quickstart, the everyday commands and options, a table of links into `docs/`, limitations, license. It must stay readable in two minutes. No phase labels (`F4`, `F11`), no changelog-style status, no exhaustive flag or command lists.
- **`docs/*.md`:** user guides and reference, organized by what a developer does (set up, devices, reports, app health, diagnostics, native lanes, commands, compatibility, install). New detail goes here, linked from the README table.
- **`CONTRIBUTING.md`:** repository layout, build from source, tests, benchmarks, and releasing.
- **`PLAN.md`:** architecture rationale, design trade-offs, and phase roadmap/status table. Never copy long usage instructions from `README.md` or `docs/` into `PLAN.md` or vice versa.
- **`packages/tapwing_driver/README.md`:** Dart driver internals, wire protocol, and binding integration.
- **`compat/README.md` & `bench/*_RESULTS.md`:** fixture harness details and reproducible benchmark logs.

## 2. Writing & Structure Rules

- **English only:** all READMEs, plans, comments, reports, and commit messages in the repo must be in English.
- **Lead with runnable commands:** keep Quick Start copy-pasteable with exact subcommands (`tapwing test`, `tapwing check`, `tapwing hierarchy`) and valid paths.
- **Tables over walls of text:** use compact Markdown tables for repository layout, CLI flags, platform parity matrices, and benchmark comparisons.
- **Keep sections tight:** prefer short bullet points with bold lead-ins; link to deep-dive files (`bench/IOS_RESULTS.md`, `compat/README.md`) instead of pasting long paragraphs into `README.md`.

## 3. Anti-Drift Audit Checklist

Run this audit before committing any documentation update:

1. **CLI flags & subcommands match code:** compare every documented subcommand and `--flag` against `crates/tapwing/src/main.rs` (`clap` structs).
2. **Referenced paths exist:** verify every linked `.md`, `.yaml`, and script path exists on disk and passes `tapwing check` for flow examples.
3. **No stale roadmap text:** grep for `"not available yet"`, `"upcoming"`, `"TODO"`, or outdated phase labels (`F2`, `F3`, ...) and update or remove anything already shipped.
4. **Accurate test & fixture counts:** match numbers in `README.md` and `PLAN.md` to actual `cargo test` + `flutter test` + `compat` counts.
5. **Measured benchmarks carry context:** whenever citing timings, include device/build mode (e.g. `emulator-5554`, profile APK) and baseline (`Maestro 2.12.1`).

## 4. Quick Verification Command

```sh
cd crates/tapwing && cargo run -- check ../../examples/sample_app/.maestro/ && grep -nE "not available yet|F[0-9]+[a-z]? " ../../README.md ../../docs/*.md || true
```

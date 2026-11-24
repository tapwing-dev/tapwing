---
name: rust-modular-design
description: >-
  Guides idiomatic, modular, maintainable Rust architecture and code reviews in
  Tapwing (`crates/tapwing`). Use when writing new Rust modules, refactoring
  large files, designing error types, or reviewing Rust code for clarity,
  testability, and clippy/rustfmt compliance.
---

# Rust Modular Design & Cleanliness

Keep `crates/tapwing` easy to navigate, fast to compile, and testable without a connected device.

## 1. Module Size & Separation of Concerns

- **Soft ceiling:** aim for **300–550 LOC** per source file (excluding `#[cfg(test)]`). When a file crosses **~800 LOC**, split it into a directory module with focused submodules (e.g. `run/mod.rs`, `run/hybrid.rs`, `run/launch.rs`, `device/android/permissions.rs`, `device/android/recording.rs`).
- **Pure core vs. I/O shell:** keep parsing, command/script builders, state transitions, coordinate math, and response parsers as pure functions taking `&str` / structs rather than `&self` with sockets or `Command`. Unit-test pure functions directly without mocking OS processes.
- **Single responsibility per function:** keep functions concise (~40–60 lines max where practical). Extract named helpers instead of nesting >3 levels of `match`/`if`.
- **Minimize visibility:** prefer private or `pub(crate)` / `pub(super)` over `pub` unless part of the crate's intentional public API.

## 2. Error Handling & Type Safety

- **Zero panics in library/CLI code:** never use `.unwrap()`, `.expect()`, or unchecked slice indexing (`xs[i]`) outside `#[cfg(test)]`. Propagate with `?` + `.context(...)` / `.with_context(...)`.
- **Typed domain errors vs. transport errors:**
  - Use `MaestroError` (with explicit `MaestroErrorKind`) for expected Maestro-level failures (`ElementNotFound`, `AssertionFailure`, `InvalidCommand`, `UnableToCopy`, etc.) so `optional: true` and `retry` behave identically to Maestro 2.12.1.
  - Keep transport deaths, socket disconnects, malformed JSON, and invalid numeric fields as plain `anyhow::Error` so they are never swallowed by `optional` or `retry`.
- **Explicit numeric conversions:** avoid raw `as u32`/`as i32`/`as u64` on signed or untrusted values. Clamp or convert explicitly (`x.max(0) as u32`, `u64::try_from(...)`, `saturating_sub`, `min(i32::MAX as u64)`).
- **Prefer enums over stringly-typed flags:** represent modes, states, and protocol responses as enums (`InputMode`, `Lane`, `AirplaneRead`), not ad-hoc strings or multiple booleans.

## 3. Formatting, Lints & Style

- Respect `crates/tapwing/rustfmt.toml` (`max_width = 120`, `use_small_heuristics = "Max"`).
- Keep all code comments, doc comments, error messages, and commit messages in **English**.
- Document **why** (invariants, Maestro source references like `AndroidDriver.kt`, protocol quirks), not what the next line obviously does.
- Delete dead code and redundant helpers immediately; never silence warnings with broad `#[allow(...)]` outside generated protobuf code.

## 4. Verification Gate

After any Rust edit or refactor, verify in one command:

```sh
cd crates/tapwing && cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test
```

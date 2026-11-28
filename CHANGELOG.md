# Changelog

All notable changes to Tapwing (`tapwing` CLI, native drivers, and `tapwing_driver`) are documented in this file. Driver-specific notes are also tracked in [packages/tapwing_driver/CHANGELOG.md](packages/tapwing_driver/CHANGELOG.md).

## Unreleased

* Driver probe (`tapwing_driver_hello_v1` + `~/.tapwing/driver-probe.json` cache) skips the 15 s handshake wait for installed builds that do not include `tapwing_driver`.
* `scrollUntilVisible` starts the next swipe once scrolling coasts below 300 logical px/s instead of waiting out the full coast settle on `BouncingScrollPhysics`.
* iOS Simulator `clearState` empties the app's data container first and falls back to reinstalling (pass `--maestro-compat strict` to always reinstall like Maestro).
* Settle no longer waits without limit for a single HTTP request or pending image: each counts as busy for at most 5 s (`--settle-load-timeout-ms` / `config.yaml` `tapwing.settleLoadTimeoutMs`, `0` = unbounded), and requests matching `--settle-http-ignore` / `tapwing.settleHttpIgnore` never block settle while still appearing in HTTP trace logs.
* Extracted shared `ui_tree`, `native_core`, Android `ImeRestore`, and iOS `HierarchyFrameMath`/`HttpConnectionMachine` modules; gated `FakeDriver` behind the `test-support` Cargo feature; consolidated Rust integration tests into a single `integration` test binary; expanded Dart layout/route snapshot parity tests.
* `retryTapIfNoChange: true` no longer taps a second time when the first tap started a platform channel call that still waits for its reply, such as a permission request whose iOS system alert leaves the app active. Before, the second tap reached the widget under the alert. A tap that changed nothing and started no such call is retried as before.
* A step whose settle ran into its timeout says what it was still waiting for: a `⚠ settle timed out, still waiting for ...` line under the step, `settleBusy` in the step's `driver` object of the JSON report and `trace.json`, and the same line in the trace viewer. A pending platform channel call is named by channel and by how long it has been pending. [Troubleshooting](docs/troubleshooting.md#a-tap-that-opens-a-system-alert-or-system-sheet-on-ios-takes-3-s) covers a tap that opens an iOS system alert and takes 3 s, with the per-step `waitToSettleTimeoutMs` workaround. The default settle timeout is unchanged.
* Reorganized project documentation under `docs/` (`architecture.md`, `decisions.md`, `roadmap.md`, `ci.md`, `migrating-from-maestro.md`, `troubleshooting.md`), added `SECURITY.md` and `.github/ISSUE_TEMPLATE/bug_report.yml`, and stripped historical scratch artifacts and machine-local paths from benchmark outputs.

## 0.1.1

* The in-app driver stays off in Flutter release builds unless compiled with `--dart-define=TAPWING_ALLOW_RELEASE=true`. Profile and debug builds are unchanged.
* `--trace` HTTP log: URLs inside header values (`Location`, `Referer`, `Content-Location`, `Link`, `Refresh`) are redacted like the request URL, the driver caps incoming request lines at 4 MiB, and an orphaned app dials once per second after a minute without a host.
* Added `scripts/release.sh` two-step release helper (`prepare` + `tag`), bundled driver `.sha256` runtime verification, and clean `SIGINT`/`SIGTERM` device teardown.

## 0.1.0

* Initial release of the Tapwing CLI (`tapwing`), bundled Android UiAutomator (`dev.tapwing.driver`) and iOS Simulator XCUITest (`dev.tapwing.ios-runner`) native drivers, and the version-locked `tapwing_driver` Flutter package.
* Full Maestro 2.12.1 YAML parser parity, embedded QuickJS-ng runtime, workspace & multi-device sharding, hybrid Flutter + native execution, reactive settle, F7 app health diagnostics, failure screenshots/"Did you mean" trees, and offline `--trace` HTML/JSON viewer.

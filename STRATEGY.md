# Tapwing Strategy

## Vision

<!-- vision: author-approved 2026-11-25 -->

`tapwing` exists so that teams can run their existing Maestro flows fast enough to gate every change, without trading away black-box honesty.
It serves QA engineers and app developers who already write Maestro YAML, and it turns those flows into exact, synchronized runs with diagnostics a human can act on.
It owns exactly one thing: executing Maestro flows against Flutter and React Native apps on Android and iOS, as one Rust binary.

### Maestro in, more out

A flow that runs in Maestro 2.12.1 runs in tapwing unchanged, with the same syntax, timeouts and selector rules.
Parity is proven by fixtures and differential runs against real Maestro, not by reading docs.
Maestro CLI flags and `MAESTRO_*` variables are accepted; flags tapwing cannot honor are ignored with a named warning, never an error.
Tapwing may add commands Maestro lacks when an in-app driver can do something Maestro cannot, including waits, control flow and app hooks.
A flow that uses them is a tapwing flow; the commands are not limited to what Maestro could safely skip.
Tapwing may change a Maestro default it can show is wrong, documented as an intentional difference with a strict-compat escape hatch.
A command tapwing cannot run faithfully parses cleanly and fails at run time with a clear message, as the AI commands do.

### Black-box tests, gray-box driver

Flows see only what the user and accessibility see, unless they explicitly opt out.
The in-app driver reads the semantics tree and injects input at the engine boundary the OS uses; it never calls handlers, reads state, or scrolls controllers directly on its own.
The driver is a dev dependency behind its own entrypoint, and `tapwing init` never edits production code, even when that means printing instructions instead.
Flows may seed state or skip setup through app hooks the app registers itself, because tapping through login in every flow is waste.
Anything that breaks the black-box contract - app hooks, turbo mode, internal selectors - is configurable per run or for a whole team in a committed `config.yaml`, and every run that uses it is flagged in the report.

### Speed from synchronization, not shortcuts

Tapwing is fast because it knows exactly when the UI has settled, not because it sleeps less or skips work.
Performance work removes waste - redundant probes, repeated parsing, oversized binaries - and states before and after numbers.
The native lane is used only for UI outside the app's own framework, is paid for only when needed, and hands control back to the fast path.
A pure in-app flow never pays for native machinery it does not use.

### Never hang, never lose a result

Every host call, device call and driver request has a deadline, and the error names what timed out.
Recovery inside a flow is bounded: transient failures retry a bounded number of times, permanent ones stop at once.
Retrying a whole failed flow is a user setting, and a pass on retry is reported as flaky, never as clean.
A shard error, panic or interrupt still writes every requested report, marked with what happened.
Files tapwing writes are written atomically.
Fixes go to the root cause and land with a test that reproduces it, often against a fake device or real socket.

### Diagnose loudly, fail on real defects

Overflows, uncaught exceptions and crashes are reported under the step that triggered them, even when every assertion passes.
From 1.0, app health failures fail the flow by default, including in suites migrated green from Maestro, because a test runner exists to show what is wrong.
`--app-errors report` keeps the softer behavior for teams that choose it.
Setup problems are warnings with a pointer to the fix, not failures.
Silent behavior changes are bugs: fallbacks, demotions and ignored inputs are announced on stderr and in reports.
Diagnosis is scoped so it never blames the app for a test's own failure.

### Scope

Tapwing targets Flutter first and React Native (including Expo) second, each with an in-app driver.
React Native support comes before physical iOS devices.
Other native apps run through the native lane as a fallback, not as a first-class target.
Android emulators and devices, the iOS Simulator, and physical iOS devices are all in scope.
Tapwing is not a test-authoring framework in Dart, not an AI assertion engine, and for now not a hosted service or device farm.
Pre-1.0, commands and output may change, but never in a way that stops a Maestro flow from parsing and running; surfacing an app defect is not breakage.
Changes state what was verified and what was not, including whether a device was used.

A change aligns when it makes Maestro flows run faster, more reliably, or with clearer diagnostics, or adds a capability only an in-app driver can provide, without hiding what a run did.
A change should be resisted when it breaks a flow that runs in Maestro, makes an internals shortcut silent or unflagged, hides a fallback, retry or failure, or adds a target beyond Flutter, React Native and the native-lane fallback.

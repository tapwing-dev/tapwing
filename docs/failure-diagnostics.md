# Failure diagnostics and trace viewer

When a flow fails, Tapwing collects enough context so you usually don't have to re-run it locally just to see what was on screen.

## Automatic failure diagnostics

Whenever a non-optional step fails (after any `retry` attempts are spent), Tapwing captures three things at the failing step. Passing runs and `optional: true` misses pay zero overhead.

- **"Did you mean" suggestions.** Tapwing inspects the live UI tree and ranks likely matches: typos, elements in the wrong state (`enabled=false`, `checked`, `focused`, `index`), `text` vs. `id` mix-ups, and partial strings that need a regex like `.*text.*`.
- **UI hierarchy.** A cleaned-up, indented tree is printed right in the terminal (skipping empty wrapper nodes), and the full tree is saved to `<test-output-dir>/failures/<flow>_step<NNN>_hierarchy.txt`.
- **Screenshot.** Saved to `<test-output-dir>/failures/<flow>_step<NNN>_failure.png`.

All three are also included in reports: attached under `failure` in `--report` JSON, embedded inline (with a collapsible tree) in HTML reports, and written to `<system-err>` in JUnit XML without breaking Maestro's `<failure>` format.

## Trace viewer

For deeper debugging, especially in CI, pass `--trace`:

```sh
# Write trace.html and trace.json into the test output directory
tapwing test .maestro/ --trace

# Also capture a screenshot and UI tree after every step
tapwing test .maestro/login.yaml --trace --trace-screenshots steps
```

`--trace` generates a single self-contained `trace.html` (no external URLs, works offline, supports light and dark mode) along with the raw `trace.json` per shard (`trace-shard-N.*` when sharded).

Open `trace.html` in a browser to step through the run:

- Status, duration, and lane (`flutter`, `native`, `hybrid`, `host`) for every step.
- Nested `runFlow`, `repeat`, and `retry` structure, including skipped `optional` steps.
- [App health](app-health.md) issues, failure screenshots, "Did you mean" hints, and UI trees.
- Every HTTP request the app made during each step (method, URL, status, timing, sizes, errors).
- A timeline bar, quick filters (failed steps, app issues, HTTP errors), and text search.

### Network logging and privacy

When `--trace` is on, the driver observes `dart:io` HTTP requests (`HttpClient`, `package:http`, Dio) by chaining onto any `HttpOverrides` your app already uses. It never modifies requests or responses. Native HTTP clients (`cupertino_http`, `cronet_http`) are not visible to it.

To keep traces safe to upload as CI artifacts:

- Request and response bodies are never recorded.
- Credentials and fragments are stripped from URLs; token-like path segments and sensitive query parameters are replaced with `<redacted>`.
- Sensitive headers (`Authorization`, `Cookie`, `Set-Cookie`, `X-Api-Key`, and any header matching `*token*`, `*secret*`, `*auth*`, `*session*`, or `*key*`) are redacted before leaving the app and checked again by the CLI.
- URLs inside other header values (`Location`, `Referer`, `Content-Location`, `Link`, `Refresh`) get the same treatment as the request URL, so an OAuth redirect's `code` or a reset link's `token` is not recorded.
- Up to 500 requests are kept per run (if more occur, dropped entries are counted).

### Performance impact

Without `--trace`, nothing extra runs. With `--trace`, the overhead is negligible and a typical ~13-step HTML trace is under 30 KB. Adding `--trace-screenshots steps` takes a screenshot and UI tree after every step (a few hundred milliseconds per step) and embeds compact JPEG thumbnails.

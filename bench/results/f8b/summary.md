# F8b trace viewer: live verification (2026-11-22)

Design: [docs/failure-diagnostics.md](../../../docs/failure-diagnostics.md#trace-viewer). Flow: `examples/sample_app/.maestro/f8_trace_network.yaml` (Android) and `ios/f8_trace_network.yaml`, 13 steps: navigate to Playground → Network, then four requests to an HTTP server the app runs on its own loopback interface (`GET /api/products?page=1&access_token=…` with `Authorization`, `POST /api/login` with `X-Api-Key` and a JSON body, `GET /api/missing` → 404, `GET 127.0.0.1:9` → connection refused). The screen installs a non-chaining `HttpOverrides` when it opens, like an app adding certificate pinning after the binding, so the driver's log-only re-wrap is exercised.

Devices: **iOS Simulator** iPhone 18 Pro (iOS 27.2, debug build) and **Android emulator** `Pixel_6_Pro` (`emulator-5630`, API 35, arm64, profile build). Release CLI. 5 runs per mode, alternating off / `--trace` / `--trace --trace-screenshots steps` on Android.

## Correctness

| Check | iOS Simulator | Android emulator |
|---|---|---|
| Flow passes in Tapwing with `--trace` | PASS | PASS |
| Flow passes unchanged in Maestro 2.12.1 | PASS | PASS |
| Each request attributed to the `tapOn` that started it (it finishes during the following `assertVisible`) | 4/4 | 4/4 |
| App's later-installed `HttpOverrides` still applied (`user-agent: tapwing-sample/1.0`), each request logged once | yes | yes |
| Secrets in `trace.json` / `trace.html` (`grep secret`: token, bearer, api key, password body, session cookie) | 0 / 0 | 0 / 0 |
| Connection refused | `state: failed`, `SocketException: Connection refused …` | same |
| Without `--trace`: files in the test output dir | none (dir not created) | none |
| Hybrid lane (`f5_permission_dialog.yaml`, `--input auto`) | – | system-dialog tap `lane: hybrid`, flow `lane: hybrid`, `http: enabled` |
| `--input native` | – | flow passes; `http: unsupported` (no driver connection, as designed) |
| Existing flows with `--trace` (`f7_app_health`, `login_and_browse`, `negative/fail_missing_element`) | PASS / PASS / expected FAIL with inline failure screenshot; F7 trace shows 5 issue kinds and the broken image's request to port 9 | PASS / PASS / expected FAIL (see note on `configure` timeouts) |

Sample step from `trace.json` (Android):

```json
{
  "index": 7, "command": "tapOn", "description": "tapOn \"Post login\"", "status": "passed",
  "ms": 97.2, "startMs": 2860.8, "depth": 0, "lane": "flutter",
  "http": [{
    "method": "POST", "url": "http://127.0.0.1:46503/api/login", "state": "completed", "status": 200,
    "startMs": 150, "headersMs": 5, "durationMs": 6, "requestBytes": 51, "responseBytes": 11,
    "requestHeaders": { "user-agent": "tapwing-sample/1.0", "x-api-key": "<redacted>", "…": "…" },
    "responseHeaders": { "set-cookie": "<redacted>", "…": "…" },
    "request": 9
  }]
}
```

## Overhead (medians of 5 runs)

`steps(12)` is the sum of the 12 step durations after `launchApp`. In `steps` mode the capture runs after the step's own timer, so its cost shows in the flow total.

| | iOS off | iOS `--trace` | iOS `steps` | Android off | Android `--trace` | Android `steps` |
|---|---|---|---|---|---|---|
| `launchApp` | 3 115 ms | 3 041 ms | 3 145 ms | 1 607 ms | 1 731 ms | 1 619 ms |
| `steps(12)` | 1 551.2 ms | 1 555.4 ms | 1 436.2 ms | 1 456.8 ms | 1 412.2 ms | 1 322.4 ms |
| Flow total | 4 668 ms | 4 604 ms | 10 214 ms | 3 045 ms | 3 166 ms | 5 962 ms |
| **Per-step overhead vs off** | – | **+0.35 ms** | capture **+424 ms** | – | **−3.7 ms** | capture **+223 ms** |
| `trace.html` | – | 27.7 KB | 350.5 KB | – | 27.8 KB | 328.7 KB |
| `trace.json` | – | 13.3 KB | 24.8 KB | – | ~13 KB | ~25 KB |

- `--trace` per-step cost is within run-to-run noise on both platforms (step ranges overlap: iOS off 1 543–1 557 ms vs on 1 504–1 620 ms; Android off 1 428–1 503 ms vs on 1 398–1 430 ms). Android `launchApp` with `--trace` ranged 1 601–2 011 ms vs 1 510–1 620 ms off (one 3 163 ms cold outlier). The `steps` runs also send `httpLog: true` and stayed at 1 563–1 731 ms, so this is emulator variance, not the `configure` change.
- `--trace-screenshots steps` is opt-in: one screenshot (`simctl io screenshot` / `screencap`) plus one hierarchy read per step. Full PNGs go to `trace/` (~130–310 KB each on the iPhone 18 Pro); the HTML inlines ≤ 360 px JPEG thumbnails.

## Note: `configure` timeouts on a starved emulator (not caused by `--trace`)

Four traced runs on `emulator-5630` failed in `launchApp` with `connection to app lost during configure: no response for 10 s`. Each was the first run after a pause or after another tool had used the emulator, with a very slow cold start (17–22 s, logcat: process start 6 s after `pm clear`, then no crash and no ANR). A few slow cold starts without `--trace` (13–16 s) happened to pass, which looked suspicious. So the CPU was starved on purpose (6 busy loops via `adb shell`, emulator at 400 % CPU) and `f7_app_health.yaml` was alternated 5× without and 5× with `--trace`:

| Run | without `--trace` | with `--trace` |
|---|---|---|
| 1 | `lost during configure` (22.8 s) | `lost during configure` (25.0 s) |
| 2–5 | 4× fail (incl. `lost during waitIdle`) | 4× fail (incl. 2× `lost during configure`) |

The failure is the same without `--trace`. It is the existing 10 s RPC silence timeout (§8.1 known limits) on an emulator whose app isolate cannot answer during cold start. The `configure` difference (`httpLog: true` sets a flag and checks one `HttpOverrides` identity) cannot block the isolate. Once the load was removed: 6/6 alternating runs passed, including a traced 21.6 s cold start. Other runs: 8/8 back-to-back alternating, 4/4 after a Maestro run, and the idle experiment (150 s pause before each run) gave on 1/2, off 2/2.

# F6 Head-to-Head Benchmark Results (Tapwing vs Maestro 2.12.1)

Date: 2026-11-21 · Measured on `main` at commit `8cb126f` (`cargo build --release`) against **Maestro 2.12.1**.

## Environment

- **Tapwing:** `8cb126f` (`--release`)
- **Baseline:** Maestro `2.12.1`
- **Android:** `emulator-5554` (API 35, `Medium_Phone_API_35`), `examples/sample_app` profile APK (`dev.tapwing.sample_app`)
- **iOS Simulator:** `iPhone 18 Pro` Simulator (iOS 27.2), `examples/sample_app` debug build (`dev.tapwing.sampleApp`; simulators do not support Flutter profile/release builds)
- **Methodology:** 3 runs per scenario (median reported where all 3 runs passed), warm ADB/simulator daemons; reference scripts: `bench/run_bench.sh`, `bench/ios/run_ios_bench.sh`

## Summary

| Platform | Flow | Mode | Maestro 2.12.1 | Tapwing | Speedup | Pass Rate |
|---|---|---|---:|---:|---:|---|
| Android | [`login_and_browse.yaml`](../examples/sample_app/.maestro/login_and_browse.yaml) (pure Flutter) | `--input auto` | 51.0 s | **7.60 s** | **6.7×** | 3/3 vs 3/3 |
| Android | [`login_and_browse.yaml`](../examples/sample_app/.maestro/login_and_browse.yaml) (pure Flutter) | `--input flutter` | 51.0 s | **7.51 s** | **6.8×** | 3/3 vs 3/3 |
| Android | [`login_and_browse.yaml`](../examples/sample_app/.maestro/login_and_browse.yaml) (pure Flutter) | `--input auto` + `disableAnimations: true` | 51.0 s | **5.75 s** | **8.9×** | 3/3 vs 3/3 |
| Android | [`f5_permission_dialog.yaml`](../examples/sample_app/.maestro/f5_permission_dialog.yaml) (hybrid) | `--input auto` | 16.20 s | **4.69 s** | **3.5×** | 3/3 vs 3/3 |
| iOS Simulator | [`ios/login_and_browse.yaml`](../examples/sample_app/.maestro/ios/login_and_browse.yaml) (pure Flutter) | `--input auto` | 72.3 s (1/3 pass) | **15.7 s** | **4.6×*** | **3/3** vs 1/3 |
| iOS Simulator | [`ios/f5_permission_dialog.yaml`](../examples/sample_app/.maestro/ios/f5_permission_dialog.yaml) (hybrid) | `--input auto` | 25.82 s | **12.23 s** | **2.1×** | 3/3 vs 3/3 |

\* Maestro passed only 1/3 runs on iOS `login_and_browse.yaml` (`72.31 s`), so 4.6× compares Tapwing's 3-run median against Maestro's single passing run (against Maestro's earlier 3/3 median of `62.7 s` in [`bench/IOS_RESULTS.md`](IOS_RESULTS.md), speedup is ~4.0×).

## Android (`emulator-5554`, API 35)

### Pure Flutter: `examples/sample_app/.maestro/login_and_browse.yaml`

| Runner / Config | Runs | Median Wall | Excl. `launchApp` | Speedup (Wall / Excl. Launch) | Notes |
|---|---|---:|---:|---:|---|
| Maestro 2.12.1 | 50.90 s, 51.04 s, 51.40 s | 51.0 s | 43.1 s | 1.0× / 1.0× | 3/3 pass |
| Tapwing `--input auto` | 7.21 s, 7.60 s, 7.60 s | **7.60 s** | **5.33 s** | **6.7× / 8.1×** | 0 native calls, 0 settle violations, 3/3 pass |
| Tapwing `--input flutter` | 7.33 s, 7.51 s, 7.68 s | **7.51 s** | **5.29 s** | **6.8× / 8.1×** | 3/3 pass |
| Tapwing `--input auto` + F6 `platform.android.disableAnimations: true` | 5.54 s, 5.75 s, 5.97 s | **5.75 s** | **3.63 s** | **8.9× / 11.9×** | Animation scales restored (`1.0 1.0 null → 0 0 0 → 1.0 1.0 null`), 3/3 pass |

### Hybrid Flutter + Runtime Permission Dialog: `examples/sample_app/.maestro/f5_permission_dialog.yaml`

| Runner / Config | Runs | Median Wall | Speedup | Notes |
|---|---|---:|---:|---|
| Maestro 2.12.1 | 15.65 s, 16.20 s, 16.71 s | 16.20 s | 1.0× | 3/3 pass |
| Tapwing `--input auto` | 4.58 s, 4.69 s, 5.04 s | **4.69 s** | **3.5×** | 0 settle violations, 3/3 pass |

## iOS Simulator (`iPhone 18 Pro`, iOS 27.2)

### Pure Flutter: `examples/sample_app/.maestro/ios/login_and_browse.yaml`

| Runner / Config | Runs | Wall | Excl. `launchApp` | Speedup | Notes |
|---|---|---:|---:|---:|---|
| Maestro 2.12.1 | 72.31 s, FAIL, FAIL | 72.3 s (1/3 pass) | — | 1.0× | Runs 2 & 3 failed with `scrollUntilVisible "Product 40.*"` 20 s timeout |
| Tapwing `--input auto` | 15.4 s, 15.7 s, 16.5 s | **15.7 s** (median) | **12.19 s** | **4.6×** (vs 1/3 pass) | −0.1% vs pre-F5b `12.20 s` baseline; 0 native calls, 0 settle violations, 3/3 pass |

### Hybrid Flutter + System Permission Dialog: `examples/sample_app/.maestro/ios/f5_permission_dialog.yaml`

| Runner / Config | Runs | Median Wall | Speedup | Notes |
|---|---|---:|---:|---|
| Maestro 2.12.1 | 25.43 s, 25.82 s, 26.86 s | 25.82 s | 1.0× | 3/3 pass |
| Tapwing `--input auto` | 11.24 s, 12.23 s, 13.45 s | **12.23 s** | **2.1×** | 0 settle violations, 3/3 pass |

## Parser Throughput (`tapwing check`)

- **`tapwing check compat/`:** **27.8 ms** total across all **519** flow files (544 YAML files in `compat/` — 463 Maestro fixtures + 81 probes — minus 25 non-flow workspace `config.yaml` / workspace-only files skipped by F6; improved from ~33 ms pre-F6 baseline).

## Regression Summary & Follow-Up Optimization (`26e7039`)

- **Zero regressions from F4/F5/F6:** iOS pure-Flutter execution excluding `launchApp` (`12.19 s`) is unchanged from the pre-F5b baseline (`12.20 s`), Android pure-Flutter stays 6.7–8.9× faster than Maestro, and both hybrid permission-dialog flows are 2.1–3.5× faster with 0 settle violations.
- **Android `NativeLaneHandle` startup/teardown optimization (`8c6ba1d` + `26e7039`):**
  1. Zero-subprocess loopback TCP via `AdbClient::forward_ephemeral` (falling back to the `adb` CLI only when the local ADB daemon is unavailable).
  2. Pre-clean `am force-stop` overlapped with ephemeral port forwarding, and both Maestro packages stopped concurrently (`am force-stop dev.mobile.maestro & am force-stop dev.mobile.maestro.test & wait`) during pre-clean and `InstrumentationGuard::drop`.
  3. `Condvar`-backed cancel-aware wait (`CancelSignal::wait_timeout`, `50 ms` poll interval) and adaptive readiness probe cap (`250 ms → 500 ms → 1000 ms` on timeout only) so dropping `NativeLaneHandle` wakes and cancels immediately.

### Interleaved A/B Measurement (`8cb126f` vs `26e7039`, `emulator-5554` API 35, `--release`)

Same booted emulator (`Pixel_6_Pro`, Android 15 / API 35), interleaved rounds (`teardown = wall - setupMs - flowMs`), 100% pass rate across all 30 runs:

| Flow | Variant | Rounds | Median Wall | Median Flow | Median Teardown | Delta vs `8cb126f` |
|---|---|---:|---:|---:|---:|---|
| [`f5_permission_dialog.yaml`](../examples/sample_app/.maestro/f5_permission_dialog.yaml) (hybrid) | `8cb126f` `--input auto` | 3 | 4971 ms | 4596 ms | 212 ms | — |
| [`f5_permission_dialog.yaml`](../examples/sample_app/.maestro/f5_permission_dialog.yaml) (hybrid) | `26e7039` `--input auto` | 3 | **4687 ms** | **4320 ms** | 217 ms | **−284 ms (−5.7%)** |
| [`login_and_browse.yaml`](../examples/sample_app/.maestro/login_and_browse.yaml) (pure Flutter) | `8cb126f` `--input auto` | 3 | 7570 ms | 7152 ms | 235 ms | — |
| [`login_and_browse.yaml`](../examples/sample_app/.maestro/login_and_browse.yaml) (pure Flutter) | `26e7039` `--input auto` | 3 | **7532 ms** | 7157 ms | **217 ms** | **−38 ms wall, −18 ms teardown** |
| [`login_and_browse.yaml`](../examples/sample_app/.maestro/login_and_browse.yaml) (pure Flutter) | `26e7039` `--input flutter` | 3 | 7295 ms | 6968 ms | 182 ms | floor (`+35 ms` teardown in `auto`) |
| [`f2_endless_animation.yaml`](../examples/sample_app/.maestro/f2_endless_animation.yaml) (short Flutter) | `8cb126f` `--input auto` | 5 | 5148 ms | 4569 ms | 261 ms | — |
| [`f2_endless_animation.yaml`](../examples/sample_app/.maestro/f2_endless_animation.yaml) (short Flutter) | `26e7039` `--input auto` | 5 | **5143 ms** | 4606 ms | **246 ms** | **−5 ms wall, −15 ms teardown** |
| [`f2_endless_animation.yaml`](../examples/sample_app/.maestro/f2_endless_animation.yaml) (short Flutter) | `26e7039` `--input flutter` | 5 | 4838 ms | 4450 ms | 200 ms | floor (`+46 ms` teardown in `auto`) |

- **Takeaway:** Hybrid flows gain **−284 ms (−5.7%)** from faster background warm-up (first native tap joins an already-ready driver). Pure-Flutter teardown overhead of `--input auto` over `--input flutter` is down to **+35–46 ms**; the remaining ~160–190 ms difference in `flow` time comes from emulator CPU contention while `am instrument` warms up in parallel with `launchApp`.

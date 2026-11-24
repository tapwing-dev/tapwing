# F2.1 Differential Verification Results (Tapwing vs Maestro 2.12.1)

Date: 2026-11-20 · Device: `emulator-5554` (Pixel 6 Pro, Android 15, 1440×3120 @ 560 dpi) · Flutter 3.47.5 (profile arm64) · Maestro 2.12.1 · Tapwing F2.1

- **Tapwing APK:** `bench/apks/tapwing.apk` (`-t lib/main_tapwing.dart`), run with `--check-settle`
- **Maestro APK:** `bench/apks/plain.apk` (production entrypoint, no driver)
- **Dark mode flows:** `adb -s emulator-5554 shell cmd uimode night yes`, always restored to `night no` afterwards

## Positive Flows (7/7 PASS in both Tapwing and Maestro, 0 settle violations)

| Flow | Tapwing status | Settle violations | Tapwing wall | Maestro status | Maestro wall | Speedup | Notes |
|---|---|---:|---:|---|---:|---:|---|
| `login_and_browse.yaml` | PASS (`rc=0`) | 0 | 7.88 s | PASS (`rc=0`) | 53.89 s | **6.8×** | Unchanged benchmark flow |
| `f2_selectors_and_taps.yaml` | PASS (`rc=0`) | 0 | 24.87 s | PASS (`rc=0`) | 51.61 s | **2.1×** | Includes 8.5 s `assertNotVisible` (#2), `adjustedToLatestInteraction` optional wait (8.54 s), real `waitUntilVisible` covered target, negative `traits: long-text` |
| `f2_swipe_and_scroll.yaml` | PASS (`rc=0`) | 0 | 13.58 s | PASS (`rc=0`) | 62.10 s | **4.6×** | Includes `Dismissible` swipes, vertical/horizontal `scrollUntilVisible`, and snapping `PageView` (`speed: 90`) |
| `f2_text_clipboard_keys.yaml` | PASS (`rc=0`) | 0 | 6.89 s | PASS (`rc=0`) | 69.73 s | **10.1×** | Includes `back` & `pressKey: Back` with soft keyboard up (#1), verified `Tab` focus into `field_two`, regex checks for `inputRandom*`, `Volume Up` |
| `f2_screenshots_and_theme.yaml` | PASS (`rc=0`) | 0 | 4.21 s | PASS (`rc=0`) | 16.86 s | **4.0×** | Compares against committed Maestro reference `ref_visual_badge.png`, tests `--test-output-dir` artifact resolution, optional pixel mismatch (`status: warned` + `ref_mismatch_badge_diff.png`), and optional size mismatch (`status: warned`) |
| `f2_endless_animation.yaml` | PASS (`rc=0`) | 0 | 4.52 s | PASS (`rc=0`) | 17.82 s | **3.9×** | Large (> 2 % screen) repeating `FadeTransition`: first entry classifies endless in 1.5 s, subsequent taps settle in ~95 ms (vs 6 s pre-F2.1) |
| `f2_dark_mode.yaml` | PASS (`rc=0`) | 0 | 3.09 s | PASS (`rc=0`) | 14.70 s | **4.8×** | `assertDarkMode` via `cmd uimode night` (29.7 ms) |

## Negative Flows (2/2 FAIL identically with `rc=1` in both Tapwing and Maestro)

| Flow | Tapwing status | Tapwing wall | Maestro status | Maestro wall |
|---|---|---:|---|---:|
| `negative/fail_missing_element.yaml` | FAIL (`rc=1`) | 2.94 s | FAIL (`rc=1`) | 9.84 s |
| `negative/fail_assert_not_visible.yaml` | FAIL (`rc=1`) | 3.26 s | FAIL (`rc=1`) | 10.77 s |

## Artifacts in this directory

- `tapwing-*.log` / `tapwing-*.json`: per-flow Tapwing stdout logs and JSON step reports
- `tapwing-*-artifacts/`: run artifacts (`screenshots/`, `screenshot-diffs/ref_mismatch_badge_diff.png`)
- `maestro-*.log`: per-flow Maestro 2.12.1 stdout logs
- `wall.txt`: recorded wall-clock times and exit codes for all runs

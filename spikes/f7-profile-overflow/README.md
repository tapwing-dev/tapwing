# Spike: Layout Overflow Detection in Profile Builds (F7)

**Question.** Flutter reports `A RenderFlex overflowed by …` only in debug builds: the report sits inside `assert` in `DebugOverflowIndicatorMixin`. Tapwing's Android lane runs **profile** APKs. Can the driver still detect overflows there, read-only, matching what debug mode would report, at negligible cost?

**Answer: yes. Option (a) from [PLAN.md §8.1](../../PLAN.md#81-f7-app-health-diagnostics) ships.** Date: 2026-11-21 · Flutter 3.47.5.

## How It Works

Flutter computes the overflow during layout in every build mode (`RenderFlex._overflow = max(0, -mainAxisFreeSpace)` decides whether to clip). Only the *report* is compiled out. [`OverflowDetector`](../../packages/tapwing_driver/lib/src/diagnostics/overflow_detector.dart) recomputes the value from public, already laid out geometry:

| Render object | Recomputed from | Reports like debug when |
|---|---|---|
| `RenderFlex` (`Row`, `Column`, `Flex`) | `spacing * (childCount - 1)` + children's main-axis sizes, summed in `_computeSizes` order (inflexible, then flexible) so the result is bit-identical; minus own size | size not empty; overflow > `precisionErrorTolerance`; always the trailing edge (`right`/`bottom`), as in Flutter |
| `RenderConstraintsTransformBox` (`UnconstrainedBox`, `ConstraintsTransformBox`) | child `parentData.offset & child.size` vs own rect | size not empty and `clipBehavior == Clip.none` |

- **Painted objects only.** Debug mode reports from `paint`, so the traversal follows `visitChildrenForSemantics`, which the framework implements as "children that are painted": offstage overlay entries and covered routes, `Offstage`, `IndexedStack`, viewports, and list keep-alive buckets. Two adjustments: nodes that hide only *semantics* (`ExcludeSemantics`, `IgnorePointer`/`AbsorbPointer` with `ignoringSemantics`, `Semantics(excludeSemantics:)`) are still descended into, and zero-opacity subtrees are skipped even with `alwaysIncludeSemantics`.
- **Visible only.** A finding is kept only if its bounds survive every ancestor `describeApproximatePaintClip` and the view. This drops list items that are laid out in the cache extent but not painted.
- **Messages are identical** to Flutter's (`_formatPixels` rules, edge order, `and` joining), so an issue fingerprints the same in debug (`FlutterError.onError`) and profile (detector) builds.
- Each finding carries global bounds and the first text inside (icons skipped) to locate it in reports, because profile builds have no `debugCreator`.

## Results

**Debug parity (`flutter test`, debug mode):** [`overflow_detector_test.dart`](../../packages/tapwing_driver/test/overflow_detector_test.dart) collects Flutter's own overflow reports via `FlutterError.onError` and requires the detector to return the **same render objects with the same messages**. 23 cases, all passing. 11 report (text row, column, `spacing`, `Expanded` + fixed, sub-pixel `0.500`, RTL, `UnconstrainedBox` left+right, nested ×3, `ExcludeSemantics`, partially visible list item, dialog). 10 must not report (`Offstage`, zero opacity, hidden `IndexedStack` child, cache-extent item, clipping `UnconstrainedBox`, ellipsized `Expanded`, horizontal scroll, zero-size flex, covered route, plus the clipped-out case below). Plus bounds/text and message formatting.

**Profile build on devices** (arm64 profile APK from [`main_overflow_spike.dart`](../../examples/sample_app/lib/main_overflow_spike.dart)):

| Check | `emulator-5554` (Pixel 6 Pro AVD, API 35, Flutter 3.47.5) | `4C101JEKB02816` (Pixel 8a, API 36, Flutter 3.50.0) |
|---|---|---|
| Expected gallery cases found (`SPIKE verify`) | **PASS: 9/9 found, 0 missing, 0 extra** (covered route, `Offstage`, opacity 0, hidden `IndexedStack`, cache-extent item, and clipping box correctly not reported) | **PASS: 9/9 found, 0 missing, 0 extra** |
| Oracle: detector vs Flutter's own `OVERFLOWING` flag (`RenderFlex.toStringShort`, kept in profile) on every non-empty `RenderFlex` in the tree | **PASS: 30/30 agree** (12 overflowing) | **PASS: 31/31 agree** (13 overflowing) |
| Scan cost, gallery (202 render objects, 300 scans) | p50 **25 µs**, p95 55 µs, max 1.1 ms | p50 **48 µs**, p95 433 µs, max 5.6 ms (single outlier) |
| Scan cost, busy list screen (300 scans) | 787 render objects: p50 **24 µs**, p95 133 µs, max 3.0 ms (single outlier) | 850 render objects: p50 **56 µs**, p95 306 µs, max 2.5 ms (single outlier) |
| Real sample app, scan every 1 s while Tapwing ran `login_and_browse`, `f2_selectors_and_taps`, `f2_swipe_and_scroll` (all **PASSED**) | 41 scans, ≤ 421 render objects: p50 **83 µs**, p95 243 µs, max 344 µs, **0 false positives** | 39 scans, ≤ 445 render objects: p50 **463 µs**, p95 921 µs, max 943 µs, **0 false positives** |

The acceptance criterion was < 2 ms per settle. On the emulator p95 is ~10× below it; on the physical Pixel 8a p95 is 0.31–0.92 ms and even the max across all 39 real-app scans is 0.94 ms (< 2× below the bar), with 0 false positives.

## Findings That Shape F7

1. **Intended difference from debug mode:** content painted but scrolled out of view inside a `SingleChildScrollView` is reported by debug mode immediately, and by the detector only once it scrolls into view (covered by a test). For a black-box tool, "the user can see it" is the right rule, and it attributes the issue to the step that revealed it.
2. **Debug builds use the `FlutterError.onError` path and the detector stays off** (`!kDebugMode`). Running both would report every overflow twice.
3. **Run the scan once per step, after settle.** At about 0.1 ms it can be always on in `--app-errors report`. Deduplicate by render object identity across scans, then by message fingerprint per step.
4. **`Rect.toString()` is stripped in profile/release** (`Instance of 'Rect'`), so reports need explicit formatting (`OverflowDetector.formatRect`). The same applies to any `toString`-based diagnostics in F7.
5. **`Icon` renders through `RenderParagraph`** as a private-use glyph and is skipped when picking locating text.
6. `runtimeType` names survive profile builds, but `--obfuscate` mangles them, which makes the type name in messages unreadable. Fingerprints stay stable within one build.
7. **Not covered:** third-party render objects that mix in `DebugOverflowIndicatorMixin` themselves. They are rare, and only `RenderFlex` and `RenderConstraintsTransformBox` use it in the framework.
8. **Environment gotcha:** a device-automation session (agent-device) switched the emulator's default IME to its own helper. No soft keyboard appeared, `hideKeyboard` fell back to `back`, and `login_and_browse` closed the app. That was not a Tapwing or spike issue: after the session closed, the IME returned to Gboard and all flows passed.

## Reproduce

```sh
# Debug parity
cd packages/tapwing_driver && flutter test test/overflow_detector_test.dart

# Profile gallery: verification, oracle, and scan cost
cd examples/sample_app
flutter build apk --profile --target-platform android-arm64 -t lib/main_overflow_spike.dart
adb install -r build/app/outputs/flutter-apk/app-profile.apk && adb logcat -c
adb shell am start -n dev.tapwing.sample_app/.MainActivity
adb logcat -d -s flutter | grep SPIKE

# Profile real app scanned every second while Tapwing drives it
flutter build apk --profile --target-platform android-arm64 -t lib/main_overflow_spike.dart --dart-define=SPIKE=app
adb install -r build/app/outputs/flutter-apk/app-profile.apk && adb logcat -c
(cd ../../crates/tapwing && ./target/release/tapwing test ../../examples/sample_app/.maestro/login_and_browse.yaml)
adb logcat -d -s flutter | grep 'SPIKE app'
```

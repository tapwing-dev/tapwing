# `hideKeyboard` in Maestro 2.12.1 on iOS Simulator × Flutter: Root Cause, Evidence, and Fixes

Date: 2026-11-20. Relative paths below refer to the isolated experiment workspace (`kb-fix/`), with key artifacts, flows, and patches preserved under `spikes/ios/hide-keyboard/`.

## TL;DR

Maestro's `hideKeyboard` fails on iOS with Flutter for **two independent reasons**, either of which is sufficient on its own:

1. **Dismiss mechanism does not work on Flutter (HW keyboard OFF, real soft keyboard).**
   `IOSDriver.hideKeyboard()` only performs two 50 ms micro-swipes at screen center. This dismisses the keyboard only when the app itself handles outside taps or drags (e.g., native `UIScrollView` with `keyboardDismissMode`, `onTapOutside`). On native iOS, Flutter's `TextField` intentionally **does not unfocus** on outside touch (that behavior is enabled only on desktop and web). `FlutterTextInputView` stays first responder and the keyboard stays up. Both swipes demonstrably arrive in Flutter (`down`, 9× `move`, `up`), yet focus never changes.
2. **False-positive visibility check (HW keyboard ON, no soft keyboard on screen).**
   The XCTest runner reports `isKeyboardVisible = app.keyboards.firstMatch.exists`. When a hardware keyboard is connected, UIKit leaves the soft keyboard in the accessibility tree but shifts it **below the bottom edge** of the screen (`y=918..1144` on an `874 pt` screen). `exists == true`, so Maestro fails with "Couldn't hide the keyboard" even though the screenshot shows no keyboard and Flutter reports `viewInsets.bottom = 0`. By contrast, Maestro's Android driver strips offscreen elements (`filterOutOfBounds`).

On Android the flow passes because `hideKeyboard` executes `input keyevent 4` (BACK), which dismisses the IME at the OS level regardless of app focus handling.

**Recommended fix:** add a global `onTapOutside → unfocus` override in the Flutter app. It is a tiny change, matches standard iOS UX, and passed 6/6 with stock Maestro in both keyboard modes. In parallel, an upstream Maestro patch (on-screen frame check + "Done" key fallback) was built from source and verified below.

**Tapwing decision:** do not copy Maestro's failure. `hideKeyboard` should succeed while emitting a compatibility warning, plus an opt-in strict mode emulating Maestro iOS behavior (see Section 5).

---

## 1. Environment & Methodology

| | |
|---|---|
| Maestro | 2.12.1 (Homebrew, `/opt/homebrew/Cellar/maestro/2.12.1`). Source: commit `4413a33` (read-only) |
| Xcode / runtime | Xcode 27.2, iOS 27.2 Simulator |
| Simulator | Dedicated `Tapwing KB test` (`7704461D-C662-4A5C-9A68-A725DA563821`, iPhone 18 Pro, 402×874 pt), deleted after testing |
| Flutter | 3.47.5 |
| App | Copy of `examples/sample_app` (`dev.tapwing.sampleApp`), built with `--dart-define=KB_VARIANT=…` |
| Runner port | `--driver-host-port 23911` to avoid colliding with default port 22087 |

**Per-device hardware keyboard toggle.** A helper binary (`hwkb`, calling CoreSimulator's `-[SimDevice setHardwareKeyboardEnabled:keyboardType:error:]`) toggled hardware keyboard per UDID without modifying global `com.apple.iphonesimulator ConnectHardwareKeyboard` (verified `0` before and after). Flutter confirmed the toggle: `inset=328` when OFF, `inset=0` when ON.

**In-app diagnostics.** An overlay `kb_debug` (`focus=<email|password|none> inset=<px>`) appeared in screenshots and the accessibility tree, alongside `debugPrint('KBDBG…')` logging pointer events, focus transitions, `viewInsets.bottom`, and `ScrollNotification`s via `xcrun simctl spawn <UDID> log stream`.

Minimal repro flow: `flows/repro.yaml`. Verification flow: `flows/repro_verify.yaml` (adds screenshot + `kb_debug ~ .*focus=none inset=0` assertion).

---

## 2. Root Cause & Evidence

### 2.1 Reproduction

```
$ maestro --device 7704461D-… test flows/repro.yaml
<failure>Couldn't hide the keyboard. This can happen if the app uses a custom input
or doesn't expose a standard dismiss action.</failure>
```

Unmodified app (`none`) with stock Maestro: **7/7 FAIL** at `hideKeyboard` (4× HW OFF, 3× HW ON). The `ondrag` variant failed identically (7/7).

### 2.2 Exact Maestro Implementation (commit `4413a33`)

`maestro-client/src/main/java/maestro/drivers/IOSDriver.kt:374-409`:

```kotlin
override fun hideKeyboard() {
    ...
    dismissKeyboardIntroduction(heightPoints = deviceInfo.heightGrid)
    if (isKeyboardHidden()) return@measured
    swipe(start = (50%, 50%), end = (50%, 47%), durationMs = 50)   // (201,437)->(201,410)
    if (isKeyboardHidden()) return@measured
    swipe(start = (50%, 50%), end = (47%, 50%), durationMs = 50)   // (201,437)->(188,437)
    waitForAppToSettle(null, null)
}
private fun isKeyboardHidden(): Boolean {
    val filter = Filters.idMatches("delete".toRegex())          // looks for "delete" key anywhere,
    val element = MaestroTimer.withTimeout(2000) {              // regardless of whether it is on-screen
        filter(contentDescriptor().aggregate()).firstOrNull() }?.toUiElementOrNull()
    return element == null
}
```

`maestro-orchestra/src/main/java/maestro/orchestra/Orchestra.kt:931-948`:

```kotlin
private suspend fun hideKeyboardCommand(): Boolean {
    maestro.hideKeyboard()
    if (maestro.isKeyboardVisible()) {           // -> IOSDriver.isKeyboardVisible() (IOSDriver.kt:255)
        throw MaestroException.HideKeyboardFailure("Couldn't hide the keyboard. ...")
```

This calls `XCTestIOSDevice.kt:156-158` (`client.keyboardInfo(appIds)`), handled by `maestro-ios-xctest-runner/maestro-driver-iosUITests/Routes/Handlers/KeyboardRouteHandler.swift:15-17`:

```swift
let keyboard = XCUIApplication(bundleIdentifier: appId).keyboards.firstMatch
let isKeyboardVisible = keyboard.exists          // existence only, no frame or hittable check
```

Compare with Android (`maestro-client/src/main/java/maestro/drivers/AndroidDriver.kt`):

```kotlin
// 377-389: isKeyboardVisible — crops hierarchy to screen bounds first
val filtered = it.filterOutOfBounds(width = deviceInfo.widthGrid, height = deviceInfo.heightGrid)
"com.google.android.inputmethod.latin:id" in jacksonObjectMapper().writeValueAsString(root)
// 490-496: hideKeyboard
shell("input keyevent 4") // 'Back', which dismisses the keyboard before handing over to navigation
```

### 2.3 XCUITest Hierarchy Observed by Maestro

Raw hierarchies after `tapOn Email` and `inputText` are saved in `evidence/hw{off,on}_raw_hierarchy.json`, extracted in `evidence/hw{off,on}_kb_nodes.txt`. Screen dimensions: 402×874 pt.

**HW keyboard OFF:** soft keyboard is on-screen.

```
type=  1 id='inputView'                      y=546..874
type=  1 id='SystemInputAssistantView'       y=546..590
type= 20 id='delete'      label='delete'     y=705..759
type=  9 id='Done'        label='done'       y=759..813      <- return key (TextInputAction.done)
XCUIElementType.keyboard (19): {'X': 0, 'Y': 590, 'Width': 402, 'Height': 226}
```

**HW keyboard ON:** identical subtree, shifted **below the bottom edge** of the screen.

```
type=  1 id='SystemInputAssistantView'       y=874..918   OFFSCREEN
type=  1 label='Typing Predictions'          y=874..884   OFFSCREEN
type= 20 id='delete'                         y=1033..1087 OFFSCREEN
type=  9 id='Done'                           y=1087..1141 OFFSCREEN
XCUIElementType.keyboard (19): {'X': 0, 'Y': 918, 'Width': 402, 'Height': 226}   (37 keys inside)
GET /keyboard -> {"isKeyboardVisible":true}                  (evidence/hwon_keyboard.json)
```

Not just `SystemInputAssistantView`, the **entire** soft keyboard stays in the accessibility tree offscreen — both the `delete` key (checked by Kotlin `isKeyboardHidden()`) and the `XCUIElementType.keyboard` element (checked by the Swift runner). Both checks therefore report "visible".

### 2.4 Is the Soft Keyboard Actually Visible?

Failure screenshots captured by Maestro at the exact moment `hideKeyboard` failed:

| HW OFF: `evidence/stock_hwoff_after_hideKeyboard_FAILED.png` | HW ON: `evidence/stock_hwon_after_hideKeyboard_FAILED.png` |
|---|---|
| Soft keyboard is visible. Overlay: `focus=email inset=328` | **No keyboard on screen.** Overlay: `focus=email inset=0` |

With HW ON, Maestro fails even though no keyboard is visible and Flutter receives `viewInsets.bottom == 0`. It is purely a false positive; the field remains focused with a blinking cursor, identical to Android after pressing BACK.

### 2.5 Effect of Swipes on Flutter Focus

App log from a failing run:

```
pointer PointerDownEvent kind=touch pos=(201,222)      <- tapOn "Email"
focus none -> email
keyboard inset 0 -> 313 -> ... -> 328                  <- (HW OFF)
pointer PointerDownEvent kind=touch pos=(201,437)      <- swipe 1, 50 ms
pointer PointerMoveEvent pos=(201,434) ... (9× ~3 px) ... (201,410)
pointer PointerUpEvent   pos=(201,410)
pointer PointerDownEvent kind=touch pos=(201,437)      <- swipe 2, 50 ms
pointer PointerMoveEvent pos=(200,437) ... (9×) ... (188,437)
pointer PointerUpEvent   pos=(188,437)
                                                       <- no "focus email -> none"
```

The swipes arrive in Flutter as normal touch events, yet never alter focus:

* **Flutter on iOS does not unfocus on tap outside.**
  `packages/flutter/lib/src/widgets/editable_text.dart:6876-6907` (`_EditableTextTapOutsideAction`):
  ```dart
  // The focus dropping behavior is only present on desktop platforms.
  case TargetPlatform.iOS: ...
    switch (intent.pointerDownEvent.kind) {
      case ui.PointerDeviceKind.touch:
        if (kIsWeb) { intent.focusNode.unfocus(); }     // no-op on native iOS
  ```
* **Flutter has no native `UIScrollView`** supporting `keyboardDismissMode`. The entire UI lives inside a single `FlutterView`; `FlutterTextInputView` remains first responder until the framework sends `TextInput.hide`/`clearClient` on unfocus.
* **Flutter's `keyboardDismissBehavior: onDrag` also fails when content does not overflow.**
  `single_child_scroll_view.dart:283-294` unfocuses on `ScrollUpdateNotification` with `dragDetails`, but `scroll_physics.dart:218-227` (`shouldAcceptUserOffset`) rejects drags when `minScrollExtent == maxScrollExtent` unless physics is `AlwaysScrollableScrollPhysics`. Logs confirmed 9 move events and **zero** `ScrollNotification`s.
* **Maestro's own error suggestion** ("tap on non-interactive element") also fails on Flutter: tapping static text "Welcome back" leaves the field focused (`evidence/hw*_raw_experiments_1.txt`).

### 2.6 Direct Runner HTTP Experiments (Port 23911)

With the app held in a focused-with-text state (`flows/hold.yaml`), documented in `evidence/hw{off,on}_raw_experiments_{1,2}.txt`:

| Action | HW OFF → `isKeyboardVisible` | HW ON → `isKeyboardVisible` |
|---|---|---|
| Maestro swipe 1 (201,437)→(201,410), 50 ms | true | true |
| Maestro swipe 2 (201,437)→(188,437) | true | true |
| Tap static text "Welcome back" | true | true |
| Tap empty area | true | true |
| `pressKey escape` | true | true |
| Interactive drag down over keyboard, 300 ms | true | – |
| **Tap soft key "Done"** | **false** | (key is offscreen) |
| **`pressKey return`** | **false** | **false** |

Only **return/Done** reliably dismisses the keyboard in both modes: `TextInputAction.done` triggers default `onEditingComplete` → unfocus → `TextInput.clearClient`, causing the engine to resign first responder and remove the `keyboard` node.

### 2.7 Root Cause Summary

| Simulator mode | Soft keyboard on screen? | Do swipes unfocus? | Runner reports | Result |
|---|---|---|---|---|
| HW keyboard OFF | Yes (`inset=328`) | No | `exists=true` (true) | FAIL: genuine, dismiss fails |
| HW keyboard ON | **No** (`inset=0`) | No | `exists=true` (`y=918`, offscreen) | FAIL: false positive |

Why Android passes: BACK (`input keyevent 4`) dismisses the IME at the OS level regardless of app focus handling, and visibility checks are clipped to screen bounds.

---

## 3. Tested Fixes & Results

Verification flows:
* `flows/repro_verify.yaml`: repro + assert `focus=none inset=0`.
* `flows/repro_verify2.yaml`: repro + assert `inset=0` only ("no soft keyboard on screen"), used for patched Maestro since HW ON legitimately keeps field focus (matching Android).

### 3.1 Summary Matrix

| # | Category | Fix | HW OFF | HW ON | Status |
|---|---|---|---|---|---|
| – | Baseline | Unmodified app `none`, stock Maestro | **0/4** | **0/3** | FAIL |
| a1 | App | **Global `EditableTextTapOutsideIntent` → `unfocus()` override** (`global`) | **3/3** | **3/3** | ✅ Passed + login regression 1/1 |
| a2 | App | Per-field `TextField.onTapOutside: (_) => unfocus()` (`tapoutside`) | 3/3 | 3/3 | ✅ Passed |
| a3 | App | `SingleChildScrollView(keyboardDismissBehavior: onDrag, physics: AlwaysScrollableScrollPhysics())` (`ondrag_always`) | 3/3 | 3/3 | ✅ Passed |
| a4 | App | Full-screen `GestureDetector(onTap: unfocus)` (`gesture`) | 3/3 | 3/3 | ⚠️ Passes only by accident, see below |
| a5 | App | `SingleChildScrollView(keyboardDismissBehavior: onDrag)` with default physics (`ondrag`) | 0/4 | 0/3 | ❌ |
| a6 | App | `textInputAction: done` (already the default, same as baseline) | 0/4 | 0/3 | ❌ Stock Maestro never presses return |
| b1 | Simulator | HW keyboard ON (per-device) | – | 0/3 | ❌ Changes failure to false positive |
| b2 | Simulator | HW keyboard OFF | 0/4 | – | ❌ |
| c1 | Maestro | **Full patch** (runner: frame check; client: on-screen `delete` + tap "Done" fallback) | **3/3** | **3/3** | ✅ Built from source & verified |
| c2 | Maestro | Runner patch only (frame check) | 0/2 | 2/2 | Partial |
| c3 | Maestro | Client patch only (on-screen `delete` + "Done") | 2/2 | 0/2 | Partial |
| c4 | Maestro | Full patch on field with `textInputAction: next` (`next`) | 0/1 | 1/1 | ❌ Return key id is `Next:`, intentionally not tapped |
| d1 | Flow | `pressKey: Enter` before `hideKeyboard` (`flows/workaround_enter.yaml`) | 2/2 | 2/2 | ✅ Has side effects |
| d2 | Flow | `pressKey: Enter` on field with `next` | 0/1 | – | ❌ Only moves focus to Password |
| d3 | Flow | Maestro error hint: `tapOn: "Welcome back"` (`flows/maestro_suggestion.yaml`) | 0/1 | 0/1 | ❌ |

### 3.2 (a) App-Side Fixes

**a1 (recommended).** Single app-wide override covering every `TextField`/`EditableText`:

```dart
MaterialApp(
  // ...
  builder: (context, child) => Actions(
    actions: <Type, Action<Intent>>{
      EditableTextTapOutsideIntent: CallbackAction<EditableTextTapOutsideIntent>(
        onInvoke: (intent) { intent.focusNode.unfocus(); return null; },
      ),
    },
    child: child!,
  ),
);
```

This works because `EditableText` registers its default action via `_makeOverridable(_EditableTextTapOutsideAction())` (`editable_text.dart:5811`). Logs confirm unfocus triggers immediately on `PointerDown` of the first micro-swipe. Regression (`flows/login_regression.yaml`) passed completely. User-visible behavior: tapping anywhere outside an input dismisses the keyboard (standard iOS UX, matching Flutter desktop/web), tapping another field moves focus normally, and tapping a button fires the button action normally.

**a2.** Same via per-field `onTapOutside`, but has to be added to every field.

**a3.** `onDrag` works only when combined with `AlwaysScrollableScrollPhysics`; without it (a5), Flutter never starts a drag when content fits on screen (even though Maestro's 27 pt swipe exceeds `kTouchSlop = 18`). Only works when screen center lies inside the scroll view.

**a4 (not recommended).** Passes only because Maestro's *second* (horizontal) swipe is 13 pt (< `kTouchSlop = 18 pt`), so Flutter interprets it as a tap — brittle and dependent on Maestro's exact swipe geometry.

### 3.3 (b) Simulator Settings

Neither HW keyboard mode fixes stock Maestro (OFF 0/4, ON 0/3); it only changes *why* it fails. For benchmarks and CI, keep **HW keyboard OFF** (per-device) so tests reflect a real phone and do not depend on a host machine's global `ConnectHardwareKeyboard` preference.

### 3.4 (c) Maestro Patch (Built & Verified)

Saved in `maestro-hideKeyboard.built.patch` / `maestro-hideKeyboard.patch`:

1. **Runner (`KeyboardRouteHandler.swift`):** consider the keyboard visible only when `keyboard.frame ∩ app.frame` has height ≥ 1 pt. Eliminates the HW-keyboard false positive (including physical devices with Bluetooth keyboards).
2. **Client (`IOSDriver.kt`):**
   * `isKeyboardHidden()` counts only an **on-screen** `delete` key, skipping unnecessary swipes when HW keyboard is ON.
   * After swipes, if the keyboard is still visible, taps the return key with id **`Done`** (or iPad `Hide keyboard`) when on-screen in the bottom half. Other return keys (`Next:`, `Go`, `Search`, `Send`, `Return`) have semantic side effects and are intentionally skipped.

Ablation (c2, c3) proves both halves are independent and required: runner fixes HW ON, client fixes HW OFF.

Build note: upstream runner sets `IPHONEOS_DEPLOYMENT_TARGET = 14.0`, rejected by Xcode 27.2 (minimum 15.0); overriding `IPHONEOS_DEPLOYMENT_TARGET=15.0` on `xcodebuild` resolves it (19 s runner build, 1 min 51 s `maestro-client` JAR).

**Patch limitations:** tapping "Done" triggers `onEditingComplete`/`onSubmitted`; `TextInputAction.next/go/search/send` fields still fail (c4); matching id `"Done"` should be scoped to descendants of `XCUIElementType.keyboard` (or a dedicated `/keyboard/dismiss` runner endpoint) before upstreaming; iPad "Hide keyboard" is untested.

### 3.5 (d) Flow Workaround (Stock Maestro, Unmodified App)

```yaml
- pressKey: Enter      # iOS: runner pressKey("return")
- hideKeyboard         # now succeeds
```

2/2 (OFF) and 2/2 (ON), but triggers `onSubmitted`/`onEditingComplete` and fails on `textInputAction: next` fields (d2).

### 3.6 Benchmark Side Finding

Even when Maestro's iOS `hideKeyboard` succeeds, it takes **4.1–5.9 s**: `dismissKeyboardIntroduction()` always waits a full 2 s timeout, `isKeyboardHidden()` waits another 2 s when already hidden, followed by `waitForAppToSettle`.

---

## 4. Recommendations

1. **App / `examples/sample_app`:** add global tap-outside-unfocus (a1) during the iOS phase (note: held back during Android F2 so Android flows testing `back` with an open keyboard aren't affected by tapping buttons).
2. **Benchmark simulator:** keep HW keyboard OFF per-device.
3. **Upstream Maestro:** file an issue/PR at minimum for the runner frame-intersection check (uncontroversial, brings iOS in line with Android's `filterOutOfBounds`).
4. **If the app cannot be modified:** `pressKey: Enter` before `hideKeyboard` on `TextInputAction.done` fields.

---

## 5. Implications for Tapwing: Should a Maestro-Compatible Runner Also Fail?

**Recommendation: No. By default, `hideKeyboard` should succeed**, while **logging a clear compatibility warning** and offering an opt-in strict mode.

Why not replicate the failure:
1. **There is no single "Maestro behavior" to copy.** The same flow passes on Maestro Android and fails on Maestro iOS — and on iOS it fails for two different reasons depending on a simulator menu toggle.
2. **Half the failures are a pure detection bug.** Under HW ON, no keyboard is on screen (`inset=0`, `on-screen: false`). Replicating that would reproduce a machine-preference-dependent false positive.
3. **Tapwing's `TextInput.hide` yields the exact same post-state as Maestro Android:** IME hidden, field still focused (also identical to iOS with a hardware keyboard).
4. **Compatibility should be asymmetric:** everything passing in Maestro must pass in Tapwing (no extra false failures), while anything passing in Tapwing that would fail in Maestro iOS should emit a **visible warning**, never silent divergence.

Concrete design for Tapwing:
* **Default mode:** `hideKeyboard` sends `TextInput.hide`. If an `EditableText` remains focused on iOS, log a warning explaining that stock Maestro iOS would fail with "Couldn't hide the keyboard" and recommending global tap-outside-unfocus or `pressKey: Enter`.
* **Opt-in `--maestro-compat=strict` (iOS):** cheaply emulate Maestro's two 50 ms micro-swipes in-process via `GestureBinding.handlePointerEvent`; if `FocusManager.primaryFocus` still points to an `EditableText`, fail with Maestro's exact error message. Never emulate the HW-ON false positive.
* **`isKeyboardVisible` in Tapwing:** define as `viewInsets.bottom > 0` (soft keyboard occupies screen space), matching Android Maestro and the patched iOS runner.
* **Known difference:** after `hideKeyboard`, focus remains in the field (matching Android Maestro, unlike iOS Maestro after a successful dismiss). Flows should always `tapOn` a field before `inputText` (already Maestro best practice).

---

## 6. Cleanup & Artifacts

The dedicated simulator (`7704461D-…`) was shut down and deleted after experiments; global `ConnectHardwareKeyboard` stayed `0`; installed Homebrew Maestro was left untouched. Preserved files under `spikes/ios/hide-keyboard/`:
- `REPORT.md`: this report
- `flows/`: `repro.yaml`, `repro_verify.yaml`, `repro_verify2.yaml`, `workaround_enter.yaml`, `maestro_suggestion.yaml`, `login_regression.yaml`, `hold.yaml`
- `evidence/`: raw runner hierarchies, keyboard node extracts, direct HTTP experiments, failure screenshots
- `maestro-hideKeyboard.patch`, `maestro-hideKeyboard.built.patch`: verified runner + client Maestro patches

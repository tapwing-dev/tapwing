# Phase 1 summary: standalone HID and accessibility probe

iPhone 18 Pro Max simulator, iOS 27.2, cs-CZ, Xcode 27.2, Apple silicon, Flutter 3.50.0. The probe is `bench/ios/hid-probe` on top of `crates/tapwing/native/ios_hid/shim.m`. Everything is measured unless marked inferred. Raw output is in `exp1` to `exp6`.

## Verdict

A SpringBoard alert is visible through `AXPTranslator` with no XCUITest runner alive, and an Indigo touch taps its button. The first snapshot of a new process takes 86 ms, against 2.6 to 2.8 s for a runner start. The app tree is not at parity with the runner, the accessibility flags of the simulator decide whether app trees are readable at all, typing did not register, and rotation has no HID path. The numbers support an integration that uses the accessibility tree for SpringBoard alerts only and HID for touches, swipes and buttons.

## Experiment 1: alert with no runner alive

- `flows/alert_up.yaml` is `f5_cs_lazy` cut after the request tap, run with `--input flutter`. The app and the alert stay up after `tapwing` exits. No `xcodebuild` or xctrunner process for this simulator was alive in any of the 14 cycles.
- `snapshot` returns SpringBoard as the frontmost process. The tree has the title, the message and three buttons, and the frames match the framebuffer PNG.
- `tap 220 570` dismissed the alert 14 of 14 times.
- Run B polled the app after the tap and read "Permission: granted" 8 of 8 times, median 624 ms (591 to 636) after the tap.
- Run A waited a fixed 1 s and read "granted" 5 of 6 times. In cycle 1 the alert was gone but the app still showed "unknown" after 1 s. The cause is unknown.

## Experiment 2: latency

Medians, with min to max.

| Call | Probe | Runner (step 0) |
|---|---|---|
| first `snapshot` in a new process, start to JSON, n=12 | 85.8 ms (72.9 to 88.9) | runner start 2.6 to 2.8 s |
| later `snapshot`, login screen, n=12 | 8.9 ms (8.7 to 15.0) | `viewHierarchy` about 22 ms |
| later `snapshot`, home screen, 13 elements, n=12 | 32.5 ms (32.2 to 34.7) | |
| later `snapshot --springboard`, two roots | about 21 ms | |
| first `frame`, n=12 | 86.7 ms (85.1 to 96.4) | `isScreenStatic` about 125 ms |
| later `frame`, n=12 | 21.2 ms (20.9 to 21.7) | |
| warm `tap`, hold 0, n=11 | 1.9 ms (0.1 to 4.2) | `touch` about 335 ms |
| warm `tap`, hold 10 ms, n=11 | 13.6 ms (10.5 to 15.1) | |
| warm `tap`, hold 50 ms, n=11 | 58.5 ms (52.7 to 61.7) | |
| `tap` as its own process, hold 0 | 69.5 ms on Flutter, 71.1 ms on UIKit | |

- The first accessibility request on a freshly booted simulator took 3.4 s.
- Holds of 0, 5, 10, 20 and 50 ms all registered 10 of 10 on a Flutter button and on a UIKit row once the app had 1.5 s to settle. Right after `simctl launch`, Flutter taps registered 6 to 10 of 10. The misses follow the launch, not the hold time.

## Experiment 3: parity with the runner

`capture_parity.sh` captures the runner's raw `POST /viewHierarchy` and the probe tree of the same screen, `compare.py` maps both to Maestro attributes. The mapping of the runner JSON agrees with `tapwing hierarchy` on the login screen.

| Screen | Runner | Probe | Matched | Only runner | Only probe |
|---|---|---|---|---|---|
| Location alert | 15 | 6 | 6 | 9 | 0 |
| Playground | 19 | 19 | 18 | 1 | 1 |
| Login, email focused, keyboard up | 64 | 52 | 51 | 13 | 1 |
| Settings root | 59 | 23 | 21 | 38 | 2 |
| Playground, landscape | 12 | 12 | 12 | 0 | 0 |

Counts are elements with at least one selector attribute, matched on attributes, frame and states.

Systematic differences:

1. The runner has an `Alert` node (type 7) with the title as its label. The probe has only the two static texts and the three buttons. The runner also includes the app behind the alert, the frontmost read of the probe does not.
2. The runner has an id-only node over some Flutter buttons (`login_button`, `playground_button`). The probe's buttons have no identifier there. `email_input`, `password_input` and the `nav_*` ids are present on both sides.
3. Status bar: time, signal, Wi-Fi and battery match. The runner's extra `Wi-Fi` image node is missing in the probe. No SpringBoard root was found in landscape.
4. Keyboard: the keys match in label and frame. The probe lacks the container nodes, the predictions label, four zero-width padding nodes and the dictation button.
5. UIKit: the probe gives one `AXButton` per Settings cell with the same label, id and frame as the runner's cell. The runner also has the static text child, the chevron image, the toolbar and the scroll indicator. The probe tree was flat in one run and nested in another.
6. An empty UIKit search field has an empty value in the runner tree. The probe reports the placeholder "Hledat" as the value. `AXPlaceholderValue` exists on UIKit text fields. Flutter fields have neither on both sides.
7. `focused` is false on both sides for the focused Flutter email field.
8. Offscreen Flutter list rows have the frame [0,0,0,0] in the probe tree.
9. The probe gives `AXRole` and `AXSubrole`, not XCUI element types. `checked` is untested, no switch screen was captured.
10. Landscape frames match exactly (root [0,0,956,440]).
11. `AXTitle` returned nothing anywhere and there is no traits attribute.

Selectors of the two f5 flows:

| Selector | Same element and tap point? |
|---|---|
| `tapOn id: playground_button` | No. The probe tree has no such id. The label "F2 Playground" gives the same button, centre (220,426). |
| `"Permission: unknown.*"`, `"Permission: granted.*"` | Yes. |
| `tapOn "Request Location Permission"` | Yes, centre (220,594). |
| `"Povolit jen při používání aplikace"` | Yes, same frame, centre (220,570). |
| `setOrientation` | Not available through HID. |
| `"Selectors & Taps.*"` in landscape | Yes. |

## Experiment 4: Flutter semantics without `tapwing_driver`

- On a new simulator with the plain `lib/main.dart` build, the probe was the first accessibility client. The read right after `simctl launch` returned the full Flutter tree, and so did the reads 1 s and 5 s later.
- `FlutterViewController.mm` in the local engine checkout (hash bf601093ad) sets `enabled = YES` under `#if TARGET_OS_SIMULATOR` in `onAccessibilityStatusChanged:`. Flutter 3.50.0 turns semantics on unconditionally on simulators. The answer to issue question 3 is yes.
- After an app relaunch, a read at 0.2 s returned no children and a read at 1 s returned the full tree.

Accessibility flags of the simulator:

- Before the first request the defaults had no `AccessibilityEnabled` or `ApplicationAccessibilityEnabled` key. After it both were 1. The translator switches them on.
- A runner session sets both back to 0 when it stops.
- With the flags at 0, `frontmostApplication` returns nil while an app is in front, and apps launched in that state return empty trees. Setting `translator.accessibilityEnabled` did not help.
- SpringBoard answered in every state.
- Writing both defaults with `simctl spawn ... defaults write com.apple.Accessibility` and relaunching the app restored the tree.

## Experiment 5: swipe, buttons, landscape

- `swipe` scrolled the UIKit list in Settings and the Flutter "Swipe & Scroll" list. A 300 ms swipe takes about 480 ms.
- `button home` with idb's target 0x33 did nothing on Xcode 27.2. Target 0x32 worked 3 of 3 and is the shim default now. `button lock` locks with 0x32, a second press wakes the lock screen.
- Trying further button targets (0x31, 0x34, 0x1, 0x0, 0x3) crashed `backboardd` on the main simulator. SpringBoard restarted by itself in under a minute. Arbitrary targets must not be sent.
- HID touches use the unrotated portrait space. In LANDSCAPE_LEFT a landscape point (lx, ly) is tapped at portrait (440 - ly, lx). Confirmed with the Back arrow. Other orientations were not tested.
- The `frame` PNG is the unrotated buffer and the screen size stays 440 x 956.

## Experiment 6: keyboard

- `type "Jan.Novak@example.com 123"` returned success in 426 ms and the focused email field stayed empty. The cause is unknown. Candidates are the HID target, the hardware keyboard setting of the simulator and the Czech "CZ" layout.
- HID usages cannot produce `í`, `š`, `ž`, `ť` or `ů` without dead keys (inferred from the source).

## Private API the shim uses

Loaded with `dlopen`, none linked: CoreSimulator, SimulatorKit, AccessibilityPlatformTranslation.

- CoreSimulator: `SimServiceContext +sharedServiceContextForDeveloperDir:error:`, `-defaultDeviceSetWithError:`, `SimDeviceSet -devices`, `SimDevice -UDID`, `-state`, `-deviceType`, `-io`, `-sendAccessibilityRequestAsync:completionQueue:completionHandler:`, `SimDeviceType -mainScreenSize`, `-mainScreenScale`, the io object's `ioPorts`, each port's `descriptor`, and the descriptor's `framebufferSurface` or `ioSurface`.
- SimulatorKit: class `SimulatorKit.SimDeviceLegacyHIDClient` (`-initWithDevice:error:`, `-sendWithMessage:freeWhenDone:completionQueue:completion:`), C symbols `IndigoHIDMessageForMouseNSEvent`, `IndigoHIDMessageForButton`, `IndigoHIDMessageForKeyboardArbitrary`, and idb's offsets for the single-touch message.
- AccessibilityPlatformTranslation: `AXPTranslator +sharedInstance`, `-bridgeTokenDelegate`, `-supportsDelegateTokens`, `-frontmostApplicationWithDisplayId:bridgeDelegateToken:`, `-translationApplicationObjectForPid:`, `-objectAtPoint:displayId:bridgeDelegateToken:`, `-macPlatformElementFromTranslation:`, `AXPTranslationObject -pid`, `-bridgeDelegateToken`, `AXPTranslatorResponse +emptyResponse`, and on `AXPMacPlatformElement` the role, subrole, label, value, identifier, placeholder, frame, enabled, focused, selected and children accessors plus `accessibilityAttributeValue:`.

## Crashes, hangs and flaky results

- The `backboardd` crash from arbitrary button targets.
- `frontmostApplication` returned nil with the flags at 0 and an app in front, with the keyboard up in the sample app (a hit-test fallback fixed it), on the Endless Animation screen (the fallback failed too), and once right after unlocking.
- Empty trees for about 0.2 s after an app launch, and lost taps right after `simctl launch`.
- Cycle 1 of run A in experiment 1.
- No private call hung and no simulator was restarted.

## The AXe no-go claims against this approach

| Claim from the AXe spike | Holds here? |
|---|---|
| SpringBoard alerts are invisible | No. Visible 14 of 14, frames match. |
| Czech text is mangled | Reading is intact. Typing did not register at all, and diacritics cannot be typed through HID usages (inferred). |
| elementType is missing | Yes. Role and subrole only, no alert container. |
| value is missing | Mostly present. An empty UIKit search field reports its placeholder as the value. |
| placeholder is missing | Present on UIKit text fields, as in the runner tree. |
| orientation | Frames are right in landscape. Rotation is not available, and touches, the PNG and the screen size stay in portrait space. |

## What limits the approach

1. App trees depend on the accessibility flags, which a runner session resets. SpringBoard is not affected.
2. A nil frontmost read needs a fallback or a retry.
3. Alert detection and `tapOn id:` on some Flutter buttons need other logic than with the runner tree.
4. The button target differs from idb's on this Xcode. Typing does not work. Rotation needs the runner.
5. Landscape needs transforms for touches, frames and the PNG.
6. Everything depends on private frameworks.

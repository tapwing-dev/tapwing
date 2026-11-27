# Research for issue #30: private API stability, Intel Macs, licensing

Answers to issue questions 4, 5 and 6 for the spike branch `spike/30-ios-hid-ax`. Every claim carries one of two tags.

- Verified means I ran a command on this machine. The command is named.
- Source means I read it in a public source. The link is given.

This machine is Apple silicon, macOS 27.2.1 as `sw_vers` reports it, and has only `/Applications/Xcode-27.2.0.app` (`ls /Applications | grep -i xcode`, `xcode-select -p`). Statements about Xcode 26.x, the previous major, come from sources only. I did not run anything on a simulator, a device or a HID service. The introspection program loads frameworks and reads the Objective-C runtime and nothing else. Raw output sits next to this file, see "Files".

## Short answers

1. Q4. Every class, selector and symbol the shim uses exists on Xcode 27.2, with five exceptions that matter little. The risk is not the names. Meta's idb project documents that from CoreSimulator 1155.4, shipped with Xcode 27, the simulator guest drops legacy Indigo button and keyboard events and delivers legacy touch unreliably. That explains two of the phase 1 findings, the button target and the typing failure. The Indigo struct layout and the button target constant `0x33` did not change in idb's history. No public source records a change of the button target value.
2. Q5. Xcode 27 installs and runs only on Apple silicon, so no Intel Mac can run Xcode 27 or an iOS 27 simulator. Intel Macs stop at Xcode 26.6, which predates the Indigo suppression. The shim has no architecture-specific code. The risk of an Intel build is that nobody has measured the shim on Xcode 26, and that an Intel binary under Rosetta cannot load the arm64e-only frameworks. Recommendation: compile the shim out for `x86_64-apple-darwin`.
3. Q6. The two MIT texts in `THIRD_PARTY_NOTICES.md` are identical to the upstream license files, and the file headers carry the copyright lines. What is missing is the release tarball. `scripts/dist.sh` does not copy this notice file, so a tarball with the shim has no idb or agent-phone notice today.

## Q4. Stability of the private API

### 4.1 The list in phase1/summary.md against shim.m

I read `crates/tapwing/native/ios_hid/shim.m` in full and compared it with the "Private API the shim uses" section of `phase1/summary.md`. The summary covers the classes and the main selectors. It leaves out these items.

| Item in shim.m | Missing from the summary |
|---|---|
| Framework paths | CoreSimulator at `/Library/Developer/PrivateFrameworks/CoreSimulator.framework/CoreSimulator`. SimulatorKit at `<Xcode>/Contents/SharedFrameworks/SimulatorKit.framework/SimulatorKit`, with `<developer dir>/Library/PrivateFrameworks/SimulatorKit.framework/SimulatorKit` as the older location. AccessibilityPlatformTranslation at `/System/Library/PrivateFrameworks/AccessibilityPlatformTranslation.framework/AccessibilityPlatformTranslation`. All three are opened with `RTLD_NOW | RTLD_GLOBAL`, and the symbol lookup relies on `RTLD_GLOBAL`. |
| Device state constant | `SimDevice -state` is compared with the literal `3` (booted). |
| Setters on AX classes | `AXPTranslator -setBridgeTokenDelegate:`, `-setSupportsDelegateTokens:`, `-setAccessibilityEnabled:` and `-accessibilityEnabled`. `AXPTranslationObject -setBridgeDelegateToken:`. |
| Extra element selectors | `AXPMacPlatformElement -translation` and `-accessibilityAttributeNames`. The Mac attribute names `AXRoleDescription`, `AXTitle` and `AXHelp`, and the skip list `AXChildren`, `AXParent`, `AXWindow`, `AXTopLevelUIElement`, `AXVisibleChildren`, `AXChildrenInNavigationOrder`. |
| Bridge delegate methods | The shim's `APTranslationBridge` implements six selectors of the translator's token delegate protocol. See 4.2 for which ones still exist. |
| Swift class name | The HID client is looked up as `SimulatorKit.SimDeviceLegacyHIDClient`, a Swift module-qualified name. |
| Indigo message layout | The summary only says "idb's offsets". The shim relies on these values. |

Indigo layout and constants the shim depends on, all in `touch_message`, `ap_button` and `ap_keys`:

- `IndigoHIDMessageForMouseNSEvent(CGPoint*, CGPoint*, uint32 target, NSUInteger eventType, CGSize, uint32 edge)` is called with target `0x32`, event type 1 for down and 2 for up, size 1 by 1 and edge 0.
- The shim overwrites the x ratio at byte `0x3c` and the y ratio at `0x44` of the builder's output, then copies `0x70` bytes from `source + 0x30`.
- The hand-built single-touch message is `0xB0 + 0x90` bytes. The inner size at `0x18` is `0x90`, the type byte at `0x1c` is `2`, the event kind at `0x20` is `0xB`, the timestamp at `0x24` is `mach_absolute_time()`. The payload is copied once more at `0x20 + 0x90`, with `1` at `+0x10` and `2` at `+0x14`.
- `IndigoHIDMessageForButton(source, direction, target)` with source `0x0` for home and `0x1` for lock, direction 1 for down and 2 for up, target `0x32`. The default was idb's `0x33` before phase 1. An 80 ms pause separates down and up.
- `IndigoHIDMessageForKeyboardArbitrary(usage, direction)` with a 4 ms pause between events.
- The environment overrides `TAPWING_HID_BUTTON_TARGET` and `TAPWING_HID_DISPLAY_ID`, and the display id `0`, are Tapwing choices and not private API.

### 4.2 Check on this machine

Program: `introspect.m`. Build and run:

```
clang -fobjc-arc -Wall -O0 introspect.m -framework Foundation -o introspect
./introspect "$(xcode-select -p)"            # introspect_output.txt, arm64 process
./introspect "$(xcode-select -p)" --respond   # introspect_output_respond.txt
```

The program also runs as an arm64e process, `introspect_output_arm64e.txt`. It loads the three frameworks from the shim's paths, then reads method lists with `class_copyMethodList` and symbols with `dlsym`. By default it calls no `respondsToSelector:`, so no class initializer of the frameworks runs. `--respond` adds `class_respondsToSelector` for the named classes and gave the same result.

Verified results, from `introspect_output.txt`:

- All three `dlopen` calls succeed, from a plain arm64 process and from an arm64e process. SimulatorKit loaded from `Contents/SharedFrameworks`.
- Found with every selector: `SimServiceContext`, `SimDeviceSet`, `SimDevice`, `SimDeviceType`, `SimulatorKit.SimDeviceLegacyHIDClient`, `AXPTranslator`, `AXPTranslationObject`, `AXPTranslatorResponse`, `AXPMacPlatformElement`. That is 50 class and selector checks, all listed in the output file. The method type encodings agree with the shim's declarations, for example `sendAccessibilityRequestAsync:completionQueue:completionHandler:` is `v40@0:8@16@24@?32`.
- Found: `IndigoHIDMessageForMouseNSEvent`, `IndigoHIDMessageForButton`, `IndigoHIDMessageForKeyboardArbitrary` through `dlsym(RTLD_DEFAULT)`, all in the SimulatorKit image. Not used by the shim but present: `IndigoHIDMessageForHIDArbitrary`, `IndigoHIDMessageForScrollEvent`, `IndigoHIDMessageForKeyboardNSEvent`.
- The io object behind `SimDevice -io` has `ioPorts` on `SimDeviceIO`, `SimDeviceIOClient` and `SimDeviceIOServer`. A port's `descriptor` is on `SimDeviceIOPort`, which lives in the separate `CoreSimDeviceIO` image that CoreSimulator loads.

Five checks did not pass, in three groups. None is a break.

1. `framebufferSurface` is not a method of any class loaded without a device. It is declared in the protocol `SimDisplayIOSurfaceRenderable`. The implementing class is presumably loaded when a device is attached. I could not confirm it without touching a device, and phase 1 shows the framebuffer read works.
2. `ioSurface`, the fallback name in `main_surface`, exists in none of the three frameworks. It is probably dead code.
3. The three non-token variants of the bridge delegate methods (`accessibilityTranslationDelegateBridgeCallback`, `...ConvertPlatformFrameToSystem:withContext:postProcess:`, `accessibilityTranslationRootParent`) are not in the current protocol `AXPTranslationTokenDelegateHelper`. Its required methods are the three token variants, which the shim implements. The extra methods are harmless.

The type encoding of `-sendWithMessage:freeWhenDone:completionQueue:completion:` contains the full `IndigoHIDMessageStruct`. I decoded it with `indigo_layout_from_encoding.py`, output in `indigo_layout_output.txt`, assuming `#pragma pack(4)` as in idb's header. The result agrees with the shim.

| Field | Decoded offset | Shim |
|---|---|---|
| mach header | `0x0` to `0x18` | not touched |
| inner size | `0x18` | writes `0x90` |
| event type byte | `0x1c` | writes `2` |
| event kind, timestamp | `0x20`, `0x24` | `0xB`, `mach_absolute_time` |
| touch event start (union) | `0x30` | copies from `0x30` |
| x ratio, y ratio | `0x3c`, `0x44` | `0x3c`, `0x44` |

Two differences exist. The encoded touch event is `0x74` bytes, one `unsigned int` more than the `0x70` the shim copies. The encoded payload element is `0x9c` bytes, with extra fields after the union, while the shim uses `0x90`. The shim's touches work in phase 1 with these sizes, and idb's own `Indigo.h` records the same two facts: the trailing `I` (source, Indigo.h) and that SimulatorKit builds payloads at an `0xA0` stride while idb's hand-built single touch uses `0x90`. So the shim matches what idb does, and it is not the layout SimulatorKit itself builds.

I did not check `AccessibilityPlatformTranslation` on disk, since it lives in the dyld shared cache. The runtime check above covers it.

### 4.3 How often it changed, and what broke clients

Sources: a full read of the idb history through `git log` on a clone of https://github.com/facebook/idb (head `62b1bce`, 2026-11-25), the idb issue list, the AXe issue list and changelog, and the agent-phone repository. Xcode release dates come from https://xcodereleases.com/data.json.

Xcode releases, from that data file: 26.6 released 2026-06-25, the last 26.x. Xcode 27.0 beta 1 on 2026-06-08, 27.0 on 2026-09-14, 27.1 on 2026-10-15, 27.2 on 2026-10-29, 27.3 beta 2 on 2026-11-16.

Stable for years:

- `Indigo.h` has 20 commits since 2017-02-02. 11 of them are from 2026 and are documentation commits that decode fields (for example 2026-04-01 "Document Indigo HID wire format"). The touch struct fields, the `0x33` hardware target and the `IndigoHIDMessageFor*` builders are the same since 2017 and 2018. Source: https://github.com/facebook/idb/commits/main/PrivateHeaders/SimulatorApp/Indigo.h
- The hardware target `0x33` is in the first `Indigo.h` commit, 2017-02-02, as `ButtonEventClass2Hardware`. It was renamed to `ButtonEventTargetHardware` on 2017-06-29 with the value unchanged. Source: commits `43ee64e79`, `59a32e9ab`.
- The AX translator headers entered idb on 2020-06-29 and CoreSimulator-based accessibility on 2020-07-01, commits `21814a295` and `bee1b76d8`. The `-[SimDevice sendAccessibilityRequestAsync:...]` path in the shim is that 2020 design.

Breaks that reached clients:

| Date | Xcode | What | Source |
|---|---|---|---|
| 2022-10-16 | 14.0.1, iOS 16.0 simulator | `idb ui swipe` behaves like a tap. No cause stated. | https://github.com/facebook/idb/issues/801 |
| 2025-06-13 | 16 and later | CoreSimulator removed `SimDeviceSet.defaultSet`. The fix uses `SimServiceContext sharedServiceContextForDeveloperDir:error:`, which the shim already uses. | https://github.com/meta/idb/pull/883 |
| 2026-06-26 | 27.0 beta 2 | SimulatorKit moved from `Contents/Developer/Library/PrivateFrameworks` to `Contents/SharedFrameworks`. The shim tries both paths. | https://github.com/cameroncooke/AXe/pull/60 |
| 2026-08-31 | 27.0 beta 5 | `SimulatorKit.SimDeviceScreen` orientation probe missing, tvOS taps fail. | https://github.com/cameroncooke/AXe/issues/69 |
| 2026-09-16 | 27.0 | `axe tap` reports success and delivers nothing, because `dtuhidd` drops events before its virtual services are active. Fixed by a transport rewrite in AXe v2.0.0, November 2026. | https://github.com/cameroncooke/AXe/issues/71 |
| 2026-09-30 | 27 (CoreSimulator 1155.4) | idb stops sending Indigo touch and button input and fails loudly. See below. | https://github.com/facebook/idb/commit/b4670b5cb |
| 2026-10-16 | 27.1, tvOS 27 | idb accessibility reads fail with "no active integrated display". | https://github.com/facebook/idb/issues/981 |
| 2026-11-16 | 27.2 | AXe 1.8.0 physical taps report completion with no visible effect. Open. | https://github.com/cameroncooke/AXe/issues/100 |

The central finding is in idb's transport selection. `FBSimulatorControl/HID/SimulatorHIDTransportSelection.swift` sets `firstDTUHIDCoreSimulatorVersion = "1155.4"`. The comment on `isLegacyInputSuppressed` says that from 1155.4 the guest drops legacy Indigo input for the whole boot, "Button and keyboard events are always dropped. Touch is unreliable: dropped on some boots and intermittently within one", and that restarting `backboardd` does not restore it. Commit `b4670b5cb`, 2026-09-30, adds that the effect holds on every runtime booted under that CoreSimulator, iOS 26.x included. The idb README says the same in "HID: IndigoHID and DTUHID". The default transport there is now `dtuhidd`, a daemon in the guest that takes XPC messages and not mach structs. Sources: https://github.com/facebook/idb/blob/main/FBSimulatorControl/HID/SimulatorHIDTransportSelection.swift and https://github.com/facebook/idb/blob/main/FBSimulatorControl/README.md

CoreSimulator on this machine is 1174.21 (Verified: `PlistBuddy -c "Print :CFBundleVersion"` on its `Info.plist`), above 1155.4. The Xcode `simctl` wrapper script also pins `EXPECTED_VERSION="1174.21"`, so CoreSimulator is a system framework that Xcode installs and the shim must follow it.

What this means for the phase 1 results. These are my inferences, no source states them for Tapwing.

- Typing that returned success and typed nothing matches "keyboard events are always dropped".
- Lost taps right after `simctl launch` match "touch is unreliable". Touch used target `0x32` and worked 14 of 14 in experiment 1, so the digitizer service seems to still accept legacy messages. I cannot say how long.
- Home with `0x33` doing nothing and `0x32` working fits too. idb's `Indigo.h` says `0x33` is the hardware-button service that the sourced builder uses for home and lock. Commit `446bfdfe4`, 2026-09-07, says a message to `0x33` that should go to `0x32` is accepted and dropped with no log line. idb's current sourced-button code still uses `0x33`, and on Xcode 27 it avoids it by sending a HID Consumer usage over DTUHID, `0x40` for home and `0x30` for lock. No source says a sourced button works through `0x32`. That is a Tapwing finding from phase 1.
- Unknown targets can crash `backboardd`. idb's list of known targets in `Indigo.h` is, in decimal, 11, 12, 13, 14, 50, 51, 60, 100, 300, 301, 302. `0x31` and `0x34` are 49 and 52, outside the list. The phase 1 crash matches that, but `0x0`, `0x1` and `0x3` crashing is not explained by any source.

When and why did the button target change? I found no change. The idb value is `0x33` since 2017-02-02 and still is. Apple changed what the guest does with it, in CoreSimulator 1155.4, so the constant did not drift in the header and the service behind it stopped acting on it. Which Xcode 27 beta first dropped it I could not determine. The CoreSimulator number 1155.4 is the earliest public marker. No source maps 1155.4 to a beta build, and the earliest AXe reports are from Xcode 27.0 beta 2 and 5 in June and August 2026.

Accessibility on Xcode 27.x. idb moved its default host-side AX to a direct XPC connection to `com.apple.CoreSimulator.accessibility` in CoreSimulatorBridge, commit `24197df31`, 2026-10-13. It keeps the CoreSimulator client path of the shim, `-[SimDevice sendAccessibilityRequestAsync:...]`, as a fallback and as a backend named `ax-coresimulator`, commit `d7e8dbc78`, 2026-10-16. So Meta still treats the shim's path as working on Xcode 27. Other idb commits and AXe items match phase 1 findings: SpringBoard remediation for a zero-framed frontmost root (commits `5d981a17a`, `06803503d`, 2026-10-14, and idb issue #1000), and AXe PR #68 "accessibility read in the first seconds after boot breaking accessibility until the simulator was rebooted", 2026-08-12.

idb also records the Xcode 26.x to 27 delta in headers marked "Removed in Xcode 27 (CoreSimulator 1155.4)": the `SimDeviceIO` consumer attach and detach calls, `SimServiceContext` connect and connection-type calls, the `SimDeviceType` bundle initializers and others. None of them is in the shim's list, which the local check confirms. Example: https://github.com/facebook/idb/blob/main/PrivateHeaders/CoreSimulator/SimDeviceIO%2BRemoved.h

Previous major, Xcode 26.x, from sources only:

- idb returns the Indigo transport for any CoreSimulator below 1155.4, so legacy Indigo input is expected to work on Xcode 26.x. Source: `SimulatorHIDTransportSelection.swift` above.
- AXe keeps Xcode 26 support and validated 26.5 with iOS 26.5, README "Xcode compatibility" at https://github.com/cameroncooke/AXe. idb's `Indigo.h` was decoded against Xcode 26.2 and the `0x32` mouse target is that header's value.
- The Xcode 26 breaks I found are API gaps in idb and AXe, for example AXe issue #59, 2026-06-12, `axe drag` on 26.5 because `FBSimulatorHIDEvent` has no touch-move event, and the 2025 `defaultSet` removal. I found no 26.x break of the classes and selectors on the shim's list.
- I did not run the shim, the introspection program or anything else on Xcode 26. The CoreSimulator version number of each 26.x release I did not find.

agent-phone has no history to measure. It was created 2026-11-25, has no issues, and says "Tested on Xcode 27.2 with iOS 27.2" in its README.

### Could not determine for Q4

- Whether the shim works on Xcode 26.x. Nothing there to run.
- The first Xcode 27 beta whose guest drops the `0x33` button events.
- Whether legacy touch through `0x32` stays reliable in later 27.x builds. idb calls it unreliable.
- Whether the `framebufferSurface` descriptor class matches the shim's expectation without a device.
- Why the Xcode 14 iOS 16 swipe broke in idb issue #801.

## Q5. Intel Macs

### 5.1 Architectures in Xcode 27.2

Verified with `lipo -archs`, full list in `local_environment.txt`.

| Binary | Architectures |
|---|---|
| `SharedFrameworks/SimulatorKit.framework/SimulatorKit` | arm64e |
| `/Library/Developer/PrivateFrameworks/CoreSimulator.framework/CoreSimulator` | arm64e |
| CoreSimulator `Resources/bin/simctl`, the real binary | arm64e |
| `IndigoLegacyHIDServices.simdeviceio` plugin binary | arm64e |
| `Developer/usr/bin/xcodebuild` | arm64 |
| `MacOS/Xcode`, `Developer/usr/bin/xctrace` | arm64 |
| `/usr/bin/xcrun`, `/usr/bin/clang` | x86_64 arm64e arm64e.x1 |

Xcode's own `Developer/usr/bin/simctl` is a bash script that execs the arm64e binary. `AccessibilityPlatformTranslation` has no file on disk, so I cannot report its slices. The two system tools at the end are macOS stubs, not Xcode's. The machine has no Rosetta, `arch -x86_64 /usr/bin/true` fails with "Bad CPU type in executable".

### 5.2 Do Xcode 27 and Xcode 26 run on Intel Macs

- Xcode 27 does not. The Xcode 27 release notes, section "Intel Deprecation": "Xcode 27 will only install and run on Apple silicon Macs." Source: https://developer.apple.com/documentation/xcode-release-notes/xcode-27-release-notes. The local binaries agree, since `Xcode`, `xcodebuild` and `xctrace` are arm64 only and Xcode's `LSMinimumSystemVersion` is 26.6 (Verified: `PlistBuddy`).
- Apple's requirements page lists Xcode 27.x for "macOS Tahoe 26.6 or later" and Xcode 26.3 to 26.6 for macOS Sequoia 15.6 through Tahoe 26.x. Source: https://developer.apple.com/support/xcode/. The page lists macOS versions and no CPU.
- macOS 26 is the last release for Intel Macs, announced at WWDC 2025, and four Intel models run it. Source: https://www.macrumors.com/2025/06/09/intel-macs-no-more-updates/ and https://sixcolors.com/post/2025/06/macos-tahoe-is-the-last-release-to-support-intel/. macOS 27 is Apple silicon only per the same coverage. This machine reports macOS 27.2.1 while Apple's page gives 26.6 as the Xcode 27 minimum, so the two version schemes in my sources and in `sw_vers` do not line up. I could not reconcile them and report both as read.
- Xcode 26.x on Intel. I found no Apple page that states a CPU requirement for 26.x. The 26 release notes say "The XcodeXIP (Apple silicon only) cannot build source editor extensions", which implies a second download that is not Apple silicon only. I infer that Xcode 26.x runs on the Intel Macs that run macOS 26, and I did not verify it. This is a source-based inference.
- An Intel Mac can never run an iOS 27 simulator. iOS 27 runtimes need Xcode 27, per a comment in idb's `FBXCTestCore/TestManager/TestBundleDTXConnection.swift` line 207.

### 5.3 Is the shim architecture-specific

No.

- Verified: `grep -n -i -E "arm64|x86|__aarch|ptrauth|pack\(|__LP64__" shim.m shim.h` has no hit. The shim compiles for all three: `clang -arch x86_64`, `-arch arm64` and `-arch arm64e` with `-c` each give an object with that architecture and no warning.
- The Indigo message uses fixed-width integers and doubles only, no pointers (Verified from the decoded type encoding in `indigo_layout_output.txt`). The layout is packed to 4 bytes and so independent of the CPU. The mach message sizes are constants in the shim. Nothing in it depends on pointer width.
- Pointer authentication does not block an arm64 client. The frameworks are arm64e, and a plain arm64 process loaded them and found every symbol (Verified: `introspect_output.txt`, arm64, and `introspect_output_arm64e.txt`). Phase 1 then called the Indigo builders and the HID client from the arm64 probe and the touches worked, per `phase1/summary.md`.
- agent-phone's Apple silicon limit is a distribution choice. `install.sh` line 20 fails when `uname -m` is not `arm64`, and the README says "Requirements: an Apple silicon Mac". The code has no such dependency. Source: the agent-phone repository at commit `4a355d4`. Its author tested on Xcode 27.2 only, and Xcode 27 cannot run on Intel, so the limit follows from what they tested.
- idb ran on Intel for years. The 2017 idb issue #467 shows an `x86_64` simulator driven through the HID path (https://github.com/facebook/idb/issues/467). That means the Indigo path and the AX path work with the x86_64 slices of Xcode up to 26.x. It says nothing about 27, where Intel does not exist.

The part that does matter is the Rosetta case. `x86_64-apple-darwin` is the Intel tarball (Verified: `scripts/dist.sh` `DEFAULT_TARGETS`, `.github/workflows/release.yml` matrix). It is built on `macos-latest`, an Apple silicon runner, by cross compiling. Intel users with that tarball are on Intel Macs, but anyone on Apple silicon can run it under Rosetta. An x86_64 process cannot load an arm64e-only dylib, so the shim's `load_frameworks` would fail and report the backend as unavailable. This follows from `lipo -archs` above and from how `dlopen` treats a missing slice. I could not test it because Rosetta is not installed here.

### 5.4 Conclusion for an Intel build

Compile the shim out of `x86_64-apple-darwin`, for example with `cfg(all(target_os = "macos", target_arch = "aarch64"))` in `crates/tapwing/build.rs` and the Rust side, so the Intel binary keeps XCUITest as the only backend. Reasons:

1. On a real Intel Mac only Xcode 26.x exists. The shim has never run there, and nothing in this spike can test it.
2. Under Rosetta on Apple silicon the shim cannot load its frameworks.
3. The code would add nothing for the two cases, and the XCUITest runner already ships as a universal simulator bundle (`docs/archive/plan-history.md`, F11b).

If someone later wants the shim on Intel anyway, the code compiles for x86_64 and needs a run on an Intel Mac with Xcode 26.x, where idb says legacy Indigo works. That is a separate measurement.

### Could not determine for Q5

- Apple's exact CPU requirement for Xcode 26.x.
- Whether an x86_64 build under Rosetta fails in the way I expect, since Rosetta is not installed on this machine.
- Whether the shim runs on an Intel Mac with Xcode 26.x. No such machine here.
- The slices of `AccessibilityPlatformTranslation`.

## Q6. Licensing

### 6.1 Licenses and the existing notices

| Project | License | Copyright line | Source |
|---|---|---|---|
| idb | MIT | `Copyright (c) Meta Platforms, Inc. and affiliates.` | https://github.com/facebook/idb/blob/main/LICENSE, and the GitHub API reports `MIT` |
| agent-phone | MIT | `Copyright (c) 2026 Govijr` | `LICENSE` at commit `4a355d4`, https://github.com/govijr/agent-phone |

Verified: a script comparing the two license blocks in `crates/tapwing/native/ios_hid/THIRD_PARTY_NOTICES.md` with the upstream `LICENSE` files, after trimming trailing spaces, reports both identical. The agent-phone text comes from the local clone and the idb text from `gh api repos/facebook/idb/contents/LICENSE`.

Points about the agent-phone license:

- The repository is two days old. Its first `LICENSE` (commit `ff5fbda`, 2026-11-25) was Apache-2.0, and commit `4a355d4`, about two hours later, changed it to MIT, both on 2026-11-25. `shim.m` was added in `7b94e26`, before any license file. Tapwing pins `4a355d4`, which is MIT. Source: `gh api "repos/govijr/agent-phone/contents/LICENSE?ref=ff5fbda"` against `?ref=4a355d4`. The copyright holder is the one author, so a relicense is theirs to make. I did not check anything else about the author.
- agent-phone's own notices credit idb with an attribution and the full MIT text, but its `shim.m` header has no copyright line. Tapwing's headers add them.

Do the notices meet the MIT terms? The MIT condition is that the copyright notice and permission notice be included in all copies or substantial portions.

- `THIRD_PARTY_NOTICES.md` has the full text and the right copyright line for both projects. Met in the source tree.
- `shim.h` and `shim.m` open with the attribution, the copyright holders, the pinned commit and a pointer to `THIRD_PARTY_NOTICES.md`. Met as long as the notice file stays in the same directory. A copy of the two source files alone would lack the permission text. That is acceptable here and not a violation.
- The notice file credits idb as a pattern source for agent-phone's layout. Phase 1 and this research confirm the shim carries idb-derived constants such as `0x3c`, `0x44` and the message sizes, so keeping the idb block is right.
- Not met: the release tarball. See 6.2.

### 6.2 Where Tapwing keeps notices, and what has to change

Verified by reading the files and `git ls-files | grep -i notice`:

| Place | Content |
|---|---|
| `drivers/ios/THIRD_PARTY_NOTICES.md` | Maestro 2.12.1 (Apache-2.0) source port table and WebDriverAgent (BSD-3-Clause) pattern. |
| `drivers/android/THIRD_PARTY_NOTICES.md` | Maestro Android driver port table. |
| `compat/maestro-fixtures/NOTICE.md` | Fixture notice for the Maestro compatibility files. |
| `crates/tapwing/native/ios_hid/THIRD_PARTY_NOTICES.md` | New in the spike, agent-phone and idb. |
| `LICENSE` | Apache-2.0 text. The tarball root gets a copy. |

What `scripts/dist.sh` puts in each release tarball: `LICENSE`, `install.sh`, `bin/tapwing`, the drivers with checksums, `share/tapwing/release`, and `share/tapwing/licenses/THIRD_PARTY_NOTICES.md`. It builds the last file from a generated header, the Android driver notices when the APK ships, the iOS driver notices on `*-apple-darwin` targets, and a `cargo tree` list of crates with their declared licenses. Verified: it never reads `crates/tapwing/native/ios_hid/THIRD_PARTY_NOTICES.md`.

Changes needed so a tarball with the shim carries the notices:

1. In `dist.sh`, add a section after the iOS driver block that appends `crates/tapwing/native/ios_hid/THIRD_PARTY_NOTICES.md`, using the same `sed 's/^#/##/' ... | tail -n +2` as the other two, only for targets that compile the shim. With the Intel decision of 5.4 that means `aarch64-apple-darwin` only, so the condition is on the target name and not on `*-apple-darwin`.
2. Update the header sentence in the generated file, which says only Maestro ports are bundled, to name agent-phone and idb.
3. `build.rs` skips the shim on every non-macOS target, so the Linux tarball needs no section.
4. Extend `scripts/test-dist.sh`, which checks the notice file for "Tapwing Android driver" and "Tapwing iOS Simulator driver", with checks for the new section on `aarch64-apple-darwin` and its absence on `x86_64-apple-darwin` and Linux.
5. `crates/tapwing/Cargo.toml` has no `include` or `exclude` key, so a `cargo package` would carry `native/ios_hid/`, notices included. Nothing to change there.

`cc`, the build dependency that compiles the shim, is not in the `cargo tree -e normal` list. It is a build tool and its code is not in the binary, so it needs no entry.

### 6.3 Tapwing's own license

Tapwing is Apache-2.0: `LICENSE`, `license = "Apache-2.0"` in `crates/tapwing/Cargo.toml`, and the `dist.sh` header. MIT code can be included in an Apache-2.0 project as long as the MIT notice stays with it, which is the work in 6.2. There is no `NOTICE` file in the repository, so Apache-2.0 section 4(d) adds nothing. One point to remember: AXe (cameroncooke/AXe) is GPL-3.0 (`gh api repos/cameroncooke/AXe --jq .license.spdx_id`). Do not copy AXe source into Tapwing. Reading its issues, as this research does, is fine.

### 6.4 What Apple's agreement says, report only

The Xcode and Apple SDKs Agreement, version EA2002 dated 06/08/2026, is at https://www.apple.com/legal/sla/docs/xcode.pdf. I extracted the text and read sections 2.2 and 2.7. Section 2.2 grants an internal use license to use the Xcode Developer Tools and the macOS SDKs "to test and develop application and other software" on Apple-branded computers. Section 2.7 forbids redistributing the Apple Software "in whole or in part" and forbids copying, decompiling, reverse engineering, disassembling or creating derivative works of it, except where applicable law prohibits the restriction. The text does not contain the words "private" or "undocumented" (`grep -n -i -E "private|undocumented"` on the extracted text, no hit), so it has no rule on calling private frameworks by name. Tapwing's shim ships no Apple binary, since it opens the user's installed frameworks with `dlopen` at run time, and the introspection program reads only runtime metadata. This is a report of the text and no legal advice.

### Could not determine for Q6

- Whether other Apple agreements, such as the Apple Developer Program License Agreement, say anything about private frameworks for a tool distributed outside the App Store. I read only the Xcode and Apple SDKs Agreement.
- What the agent-phone author intended when they switched from Apache-2.0 to MIT.
- Anything about patents or other rights in the Indigo protocol.

## Files

All under `bench/results/ios-hid-spike/research/`.

- `summary.md`, this file.
- `introspect.m`, the introspection program. It builds with the `clang` line in section 4.2 and touches no simulator.
- `introspect_output.txt`, arm64 process, default mode. `introspect_output_respond.txt`, with `--respond`. `introspect_output_arm64e.txt`, arm64e process.
- `indigo_layout_from_encoding.py` and `indigo_layout_output.txt`, which decode the `IndigoHIDMessageStruct` type encoding from `introspect_output.txt`. Run `python3 -I indigo_layout_from_encoding.py introspect_output.txt 4 0`.
- `local_environment.txt`, the `lipo -archs`, `sw_vers`, version plist and Rosetta check output.

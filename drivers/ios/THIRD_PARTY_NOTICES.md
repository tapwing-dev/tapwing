# Third-party notices: Tapwing iOS Simulator driver

## Maestro (source port)

Parts of this driver are ported from the iOS XCUITest driver of
[Maestro](https://github.com/mobile-dev-inc/maestro) 2.12.1 (tag `cli-2.12.1`,
commit `67d8a677029261fa424a45a10d08eacd6aa0deed`), Copyright (c) 2022
mobile.dev inc., licensed under the Apache License 2.0
(<http://www.apache.org/licenses/LICENSE-2.0>). Maestro ships no NOTICE file.
Every ported file keeps the attribution, states that it was modified, and names
its source:

| File (`drivers/ios/`) | Maestro source (`maestro-ios-xctest-runner/maestro-driver-iosUITests/`) |
|---|---|
| `Core/Models.swift` | `Routes/Models/*.swift`, `Routes/Helpers/AppError.swift` |
| `Core/OrientationMath.swift` | `Routes/Helpers/ScreenSizeHelper.swift`, `Routes/Handlers/ViewHierarchyHandler.swift` |
| `UITests/HierarchyBuilder.swift` | `Routes/Handlers/ViewHierarchyHandler.swift` |
| `UITests/ScreenSizeHelper.swift` | `Routes/Helpers/ScreenSizeHelper.swift` |
| `UITests/TextInputHelper.swift` | `Routes/Helpers/TextInputHelper.swift` |
| `UITests/Actions.swift` | `Routes/Handlers/{TouchRouteHandler,SwipeRouteHandlerV2,InputTextRouteHandler,EraseTextHandler,PressKeyHandler,PressButtonHandler,SetOrientationHandler}.swift` |
| `UITests/ScreenProbe.swift` | `Routes/Handlers/{StatusHandler,DeviceInfoHandler,ScreenDiffHandler,RunningAppRouteHandler,KeyboardRouteHandler,ScreenshotHandler}.swift` |
| `UITests/XCTestPrivate/RunningApp.swift` | `Routes/XCTest/RunningApp.swift` |
| `UITests/XCTestPrivate/XCTestRuntimeBridge.{h,m}` | `Categories/XCUIApplication+Helper.m`, `Utilities/{AXClientProxy,XCTestDaemonsProxy}.m`, `Routes/XCTest/{AXClientSwizzler,EventRecord,EventTarget,PointerEventPath,RunnerDaemonProxy}.swift` |

## WebDriverAgent (snapshot parameter swizzle semantics)

The `snapshotKeyHonorModalViews` parameter-swizzling pattern in
`UITests/XCTestPrivate/XCTestRuntimeBridge.m` follows WebDriverAgent's
`XCAXClient_iOS+FBSnapshotReqParams.m` (as carried in Maestro 2.12.1),
Copyright (c) 2015-present, Facebook, Inc., BSD-3-Clause license.

## Third-party libraries in the runner bundle

None. The runner links exclusively against Apple iOS Simulator system frameworks
(`Foundation`, `Network`, `UIKit`, `CryptoKit`, `XCTest`).

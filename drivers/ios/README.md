# Tapwing iOS Simulator driver (`dev.tapwing.ios-runner`)

Tapwing's own iOS Simulator native-lane driver (PLAN.md §8.3, phase F11b). It
replaced Maestro's `maestro-ios-driver.jar` (no longer used by Tapwing) with a small, zero-dependency Swift
+ minimal Objective-C XCUITest HTTP runner that speaks the exact 14-route JSON
HTTP/1.1 API consumed by `crates/tapwing/src/ios_native_lane/client.rs`.

```sh
scripts/build-ios-driver.sh              # host unit tests + drivers/ios/build/* (+ .sha256)
tapwing test ...                         # always uses it
```

| | |
|---|---|
| Host app / runner bundle IDs | `dev.tapwing.ios-runner` / `dev.tapwing.ios-runnerUITests.xctrunner` |
| Scope | iOS Simulator only (`iphonesimulator`, no physical devices) |
| Start | `xcodebuild test-without-building -xctestrun tapwing-ios-runner.xctestrun -destination id=<UDID>` with `TEST_RUNNER_PORT=<port>` |
| External dependencies | None (Apple SDK only: `XCTest`, `Network`, `UIKit`, `CryptoKit`, `Foundation`) |
| Toolchain | Xcode (`xcodebuild`) + Swift (`swift test` for `Core/` host unit tests) |
| Licenses | ported Maestro / WebDriverAgent semantics: [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md) |

## What is Maestro's and what is not

Served routes (all 14 consumed by `ios_native_lane/client.rs`):
`GET /status`, `GET /deviceInfo`, `POST /viewHierarchy`, `POST /touch`,
`POST /swipeV2`, `POST /inputText`, `POST /eraseText`, `POST /pressKey`,
`POST /pressButton`, `POST /setOrientation`, `GET /isScreenStatic`,
`POST /runningApp`, `POST /keyboard`, `GET /screenshot`.

- **Same as Maestro 2.12.1** (1:1 wire and semantic parity):
  - `AXElement` snapshot walk (`XCUIElement.snapshot()`, cross-process remote
    window offsets, SpringBoard status bars, iOS 26+ `SafariViewService` WebView
    hierarchy merge, portrait/landscape frame guard, `kAXErrorIllegalArgument` /
    `kAXErrorInvalidUIElement` recovery fallback, `snapshotKeyHonorModalViews`
    support).
  - Automatic UI interruption dismissal suppressed (`doesNotHandleUIInterruptions`)
    so SpringBoard permission alerts stay visible to flows.
  - Orientation-aware touch and `swipeV2` coordinate transforms and HTTP 408
    classification for XCUITest UI-query / main-thread-busy timeouts.
  - Paced `inputText` (ASCII + full Unicode, including Czech diacritics),
    `eraseText`, `pressKey`, `pressButton` (`home`, `lock`), `setOrientation`,
    `isScreenStatic` (SHA-256 PNG diff), `runningApp`, `keyboard`, `screenshot`.
- **Different:**
  - Zero third-party Swift packages (no FlyingFox, no MaestroDriverLib
    framework): uses a ~150-line `Network.framework` (`NWListener`) keep-alive
    HTTP/1.1 server with `TCP_NODELAY`.
  - No dumped Apple private header tree: uses one self-contained Objective-C
    runtime bridge (`UITests/XCTestPrivate/XCTestRuntimeBridge.{h,m}`) with ARC
    memory management and a single unified `defaultParameters` swizzle.
  - Unused Maestro routes (`launchApp`, `terminateApp`, `setPermissions`,
    `setAppearance`, `swipe` v1, `eraseAllText`) are omitted.

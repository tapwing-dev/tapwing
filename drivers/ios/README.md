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
| Start | `xcodebuild test-without-building -xctestrun tapwing-ios-runner.xctestrun -destination id=<UDID>` with `TEST_RUNNER_PORT=<port>` and `TEST_RUNNER_TAPWING_SESSION=<id>` |
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
  - Per-request deadline and a wedge-proof `/status` (not in Maestro): see
    [Threading and deadlines](#threading-and-deadlines).
  - No dumped Apple private header tree: uses one self-contained Objective-C
    runtime bridge (`UITests/XCTestPrivate/XCTestRuntimeBridge.{h,m}`) with ARC
    memory management and a single unified `defaultParameters` swizzle.
  - Unused Maestro routes (`launchApp`, `terminateApp`, `setPermissions`,
    `setAppearance`, `swipe` v1, `eraseAllText`) are omitted.

## Threading and deadlines

XCUITest calls run on the main thread and cannot be cancelled, so the runner
never pretends to stop one. It keeps answering instead, and leaves the actual
recovery (a restart of the runner process) to the host.

- **Server queue** (`dev.tapwing.ios-runner.http`, serial): accepts connections,
  parses requests, sends every response, runs the deadline watchdogs and
  answers `GET /status` itself. It never waits for the main thread.
- **Main actor**: runs one route handler at a time, in arrival order, behind
  `RequestGate` (`Core/RequestGate.swift`).
- **Deadline**: the host sends its request budget in `X-Tapwing-Deadline-Ms`;
  the runner keeps a 2 s margin below it (25 s when the header is missing).
  When it passes, the watchdog answers HTTP 408 with `errorCode`
  `deadlineExceeded` (the handler is still running and may have applied the
  request) or `runnerBusy` (the request never started). The overdue handler
  keeps the gate until it returns, so XCUITest operations never interleave.
- **Wedged**: while an overdue handler is still running, new requests are
  refused at once with `runnerBusy` and `/status` answers
  `{"status":"wedged","busy":"POST /viewHierarchy","busyMs":31000,"wedged":true}`
  (still HTTP 200). If the handler does return, the gate heals by itself.
- **Dead requests**: a queued request whose client hung up, or whose deadline
  passed while it waited, is never started.
- **Session id**: `/status` echoes `TEST_RUNNER_TAPWING_SESSION` as
  `"session"`. The host waits for its own id, so a runner left over from a
  hard-killed run that still answers on the same port is never mistaken for
  the one just started. A runner started without the variable omits the field.
- **Typing**: `inputText` / `eraseText` type the slow first character, then
  chunks of 60 characters (`Core/TypingPlan.swift`), and stop between chunks
  once the request is dead. Text of up to 61 characters is still one event,
  as in Maestro. The host scales the deadline of these two routes with the
  text length, so long text is not cut off.

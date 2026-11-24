# F11b results: Tapwing iOS Simulator driver vs Maestro 2.12.1's

Setup: iOS Simulator `Tapwing-AXe` (iPhone 18 Pro, 402×874 pt / 1206×2622 px @3x,
iOS 27.2, `en_US`), Xcode 27.2 (`27B52`), Apple Silicon Mac host.
Sample app: debug build of `examples/sample_app` (`dev.tapwing.sampleApp`).
Tapwing iOS driver: universal `arm64 + x86_64` simulator build from `drivers/ios/`
(`scripts/build-ios-driver.sh`, zero external Swift dependencies).
Maestro baseline: Maestro 2.12.1 (`maestro-ios-driver.jar`, `driver-iPhoneSimulator/`).
Date: 2026-11-21.

## 14-endpoint & keep-alive verification

Verified live on `Tapwing-AXe` over a **single persistent HTTP/1.1 keep-alive socket**:

| Endpoint | Verified behavior |
|---|---|
| `GET /status` | `200 {"status":"ok"}` |
| `GET /deviceInfo` | `402×874 pt` / `1206×2622 px` portrait; swaps to `874×402 pt` in landscape |
| `POST /viewHierarchy` | `depth=12`, exact `AXElement` match vs Maestro 2.12.1 (`only_tapwing=0, only_maestro=0`) |
| `POST /touch` | Focuses Flutter fields and taps SpringBoard permission alert buttons |
| `POST /swipeV2` | Vertical, horizontal, `Dismissible`, and `PageView` swipes; rejects negative duration with HTTP 400; classifies XCTest timeouts as HTTP 408 |
| `POST /inputText` | ASCII + Czech Unicode (`Příliš žluťoučký kůň`) verified verbatim in Flutter text field |
| `POST /eraseText` | Erases exact character count (`Příliš žluťoučký kůň` → `Příliš žluťoučký`) |
| `POST /pressKey` | `return`, `delete` verified |
| `POST /pressButton` | `home` returns to `com.apple.springboard`; `lock` wired via `-[XCUIDevice pressLockButton]` |
| `POST /setOrientation` | `portrait`, `landscapeLeft`, `landscapeRight`, `upsideDown` all return 200 and rotate as supported by iOS |
| `GET /isScreenStatic` | Returns `{"isScreenStatic": bool}` via SHA-256 PNG comparison |
| `POST /runningApp` | Tracks `dev.tapwing.sampleApp` vs `com.apple.springboard` |
| `POST /keyboard` | Reports `true` when soft keyboard is visible, `false` when hidden |
| `GET /screenshot` | Valid PNG (`compressed=false`, ~98 KB) and JPEG (`compressed=true`, ~84 KB) |
| Unknown route | Returns `404` with `{"errorMessage":"...","errorCode":"notFound","code":"notFound"}` |

## End-to-end differential flows (`tapwing test`)

| Flow | Mode / Driver | Result | Notes |
|---|---|---|---|
| `ios/f5_permission_dialog.yaml` | `--native-driver tapwing --input auto` | **PASS** (9.3 s checkout / 7.1 s bundled) | Real SpringBoard location alert + `LANDSCAPE_LEFT`/`PORTRAIT` |
| `ios/f5_permission_dialog.yaml` | `--native-driver tapwing --input native` | **PASS** (8.6 s) | Entire flow driven via Tapwing's iOS XCUITest runner |
| `ios/f5_permission_dialog.yaml` | `--native-driver maestro --input auto` | **PASS** (7.6 s) | Baseline parity match |
| `ios/login_and_browse.yaml` | `--native-driver tapwing --input auto` | **PASS** (15.0 s) | `lane: flutter`, `laneStats: {flutter: 27, native: 0}` (`IosNativeSlot::Lazy` untouched) |
| `ios/login_and_browse.yaml` | `--native-driver tapwing --input native` | **PASS** (36.3 s) | All 27 steps native (tap, text, hideKeyboard, scrollUntilVisible, switch) |
| `ios/f2_swipe_and_scroll.yaml` | `--native-driver tapwing --input native` | **PASS** (46.3 s) | Dismissible swipes, vertical/horizontal lists, PageView snap |
| `ios/f2_text_clipboard_keys.yaml` | `--native-driver tapwing --input native` | **PASS** (40.4 s) | `copyTextFrom`, `pasteText`, `eraseText`, `pressKey`, `setClipboard`, `inputRandom` |
| `ios/f2_hide_keyboard.yaml` | `--native-driver tapwing --input native` | **PASS** (10.5 s) | Soft keyboard show/hide cycles |
| `ios/f2_ios_device_commands.yaml` | `--native-driver tapwing --input native` | **PASS** (6.4 s) | `setPermissions`, `setLocation`, `clearKeychain`, `launchApp` |

## Release tarball self-containment

Built with `scripts/dist.sh --target aarch64-apple-darwin --no-android-driver`,
extracted to a clean directory, verified `shasum -a 256 -c SHA256SUMS` (8/8 OK),
checked `tapwing --version` (`tapwing ios driver: 0.1.0 (bundled)`) with all
`TAPWING_*` env vars unset, and ran `ios/f5_permission_dialog.yaml` with
`--native-driver tapwing --input auto` (**PASS in 7.1 s**, atomically unpacked
to `~/.tapwing/ios-native-driver/tapwing-<hash>/` with `.tapwing-complete`).

## Timings & bundle size

| Metric | Maestro 2.12.1 driver | Tapwing iOS driver |
|---|---|---|
| Cold start (`xcodebuild` to `/status` 200) | 2 866 ms | **2 480 ms** |
| `POST /viewHierarchy` median (10 calls) | 43.0 ms | **41.9 ms** |
| `POST /touch` median (10 calls) | 349.3 ms | **349.9 ms** |
| `GET /screenshot?compressed=false` median (5 calls) | 48.0 ms | **48.7 ms** |
| Hierarchy diff (login screen, excluding status bar clock) | 12 nodes | **12 nodes (0 diff)** |
| Bundled archive size (`xctestrun` + 2 zips, `arm64 + x86_64`) | ~7.3 MB (jar: ~20 MB) | **375 KB** (~19.5× smaller) |

# Tapwing Android driver (`dev.tapwing.driver`)

Tapwing's own Android native-lane driver (see [docs/architecture.md](../../docs/architecture.md) and [docs/decisions.md](../../docs/decisions.md)). One APK
replaced Maestro's two (`maestro-app.apk` + `maestro-server.apk`, no longer
used by Tapwing): its instrumentation targets its own package, runs UiAutomator
and serves the same gRPC API Maestro's driver did
(`crates/tapwing/proto/maestro_android.proto`).

```sh
scripts/build-android-driver.sh          # unit tests + drivers/android/build/tapwing-driver.apk (+ .sha256)
tapwing test ...                         # always uses it
```

| | |
|---|---|
| Package / instrumentation | `dev.tapwing.driver` / `dev.tapwing.driver/.TapwingDriverInstrumentation` |
| Start | `am instrument -w -m -e port 7001 dev.tapwing.driver/.TapwingDriverInstrumentation` |
| SDK | minSdk 24 (as Maestro), compile/target 37 |
| Toolchain | Gradle 9.9.2 wrapper (SHA-256 pinned), AGP 9.5.2, JDK 17+ |
| Signing | committed public test key, see [signing/README.md](signing/README.md) |
| Licenses | ported Maestro code and bundled libraries: [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md) |

## What is Maestro's and what is not

Served RPCs: `deviceInfo`, `viewHierarchy`, `tap`, `inputText`, `eraseAllText`,
`screenshot`, `isWindowUpdating`, `setLocation`, `enableMockLocationProviders`,
`disableLocationUpdates`. `launchApp` and `addMedia` (unused by Tapwing)
answer `UNIMPLEMENTED`.

- **Same as Maestro 2.12.1** (ported code, same UiAutomator 2.3.0):
  hierarchy XML (window roots via `UiDevice.getWindowRoots`, attribute set and
  order, visible bounds, WebView children, toast node, the 500 ms
  `waitForIdle` + accessibility cache reset before each dump), taps
  (`clickNoSync`, 100 ms), ASCII `inputText` (one key press per character,
  75 ms apart), `eraseAllText` (`KEYCODE_DEL` without animation waits),
  screenshots, `isWindowUpdating` (`waitForWindowUpdate(appId, 500)`), mock
  location providers.
- **Different:** non-ASCII `inputText` is handled on the device without
  switching the IME (`ACTION_SET_TEXT` reproducing `commitText` at the cursor;
  an in-APK IME only as fallback). See `input/UnicodeTextInput.kt`.
  `inputText` and `eraseAllText` look at the gRPC call before every key (or
  IME text chunk) and stop, answering `CANCELLED`, once the client reset the
  call, its deadline passed or the connection died; Maestro's driver types
  the rest regardless (`input/CancellableLoop.kt`). The CLI sends ASCII text
  in pieces of 100 characters, so that typing at 75 ms per key never
  outlasts its 30 s RPC deadline.
  `enableMockLocationProviders` grants this package the mock-location app-op
  and location permissions itself. The gRPC server is grpc-okhttp instead of
  grpc-netty-shaded (same HTTP/2 wire, 4 MB APK instead of 12.6 MB) and
  listens on the device's IPv4 loopback only (`adb forward` reaches it there),
  not on every interface.
- **Cleaning up after a killed run:** before the IME fallback selects the
  in-APK keyboard, the driver records the previous one in `settings` (global,
  `tapwing_ime_restore`); the CLI's stop command (`native_lane/command.rs`)
  selects it again when the driver was killed before it could. Mock location
  left behind by a killed driver is removed when the next one starts, and the
  mock-location app-op is handed back when the providers are disabled.

# Third-party notices: Tapwing Android driver

## Maestro (source port)

Parts of this driver are ported from the Android driver of
[Maestro](https://github.com/mobile-dev-inc/maestro) 2.12.1 (tag `cli-2.12.1`,
commit `67d8a677029261fa424a45a10d08eacd6aa0deed`), Copyright (c) 2022
mobile.dev inc., licensed under the Apache License 2.0
(<http://www.apache.org/licenses/LICENSE-2.0>). Maestro ships no NOTICE file.
Every ported file keeps the original copyright line, states that it was
modified, and names its source:

| File (`driver/src/main/kotlin/`) | Maestro source |
|---|---|
| `androidx/test/uiautomator/UiDeviceAccess.kt` | `maestro-android/src/androidTest/java/androidx/test/uiautomator/UiDeviceExt.kt` |
| `dev/tapwing/driver/DriverService.kt` | `maestro-android/src/androidTest/java/dev/mobile/maestro/MaestroDriverService.kt` |
| `dev/tapwing/driver/TapwingDriverInstrumentation.kt` | same (`MaestroDriverService.grpcServer`) |
| `dev/tapwing/driver/hierarchy/ViewHierarchy.kt`, `XmlChars.kt` | `.../dev/mobile/maestro/ViewHierarchy.kt` |
| `dev/tapwing/driver/hierarchy/AccessibilityNodeInfoExt.kt` | `.../dev/mobile/maestro/AccessibilityNodeInfoExt.kt` |
| `dev/tapwing/driver/hierarchy/ToastListener.kt` | `.../dev/mobile/maestro/ToastAccessibilityListener.kt` |
| `dev/tapwing/driver/input/InputInjector.kt` | `.../dev/mobile/maestro/InputInjector.kt` |
| `dev/tapwing/driver/input/AsciiKeys.kt` | `MaestroDriverService.kt` (`Service.setText`) |
| `dev/tapwing/driver/input/TextChunks.kt` | `maestro-client/src/main/java/maestro/drivers/AndroidDriver.kt` |
| `dev/tapwing/driver/input/TapwingInputMethodService.kt` | `maestro-android/src/main/java/dev/mobile/maestro/input/MaestroInputMethodService.kt` |
| `dev/tapwing/driver/screenshot/Screenshots.kt` | `.../dev/mobile/maestro/screenshot/ScreenshotService.kt` |
| `dev/tapwing/driver/location/MockLocation.kt` | `.../dev/mobile/maestro/location/*.kt` |

The wire format, `crates/tapwing/proto/maestro_android.proto`, is Maestro's
`maestro-proto/src/main/proto/maestro_android.proto`, unmodified.

## Libraries in the APK

| Library | License |
|---|---|
| gRPC Java (`io.grpc:grpc-api`, `-core`, `-okhttp`, `-protobuf-lite`, `-stub`, `-util`, `-context`) | Apache-2.0 |
| Protocol Buffers (`com.google.protobuf:protobuf-javalite`) | BSD-3-Clause |
| Okio (`com.squareup.okio`) | Apache-2.0 |
| Guava, failureaccess, listenablefuture, Gson, error-prone annotations, j2objc annotations, `com.google.android:annotations` | Apache-2.0 |
| JSR-305 annotations (`com.google.code.findbugs:jsr305`) | BSD-3-Clause |
| PerfMark API, animal-sniffer annotations | Apache-2.0 / MIT |
| AndroidX (UiAutomator, test monitor, core, lifecycle, fragment, ...) | Apache-2.0 |
| Kotlin standard library, kotlinx.coroutines, JetBrains annotations | Apache-2.0 |
| JSpecify annotations | Apache-2.0 |
| JUnit 4 (pulled in by UiAutomator), Hamcrest core | EPL-1.0, BSD-3-Clause |
| Google Play services (`play-services-location`, `-base`, `-basement`, `-tasks`) | Android Software Development Kit License |

Regenerate the list with
`./gradlew :driver:dependencies --configuration releaseRuntimeClasspath`.

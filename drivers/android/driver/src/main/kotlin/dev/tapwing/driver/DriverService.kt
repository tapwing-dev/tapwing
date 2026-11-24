// Copyright (c) 2022 mobile.dev inc.
// Modifications copyright (c) 2026 the Tapwing authors.
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.
//
// Ported from Maestro 2.12.1 (github.com/mobile-dev-inc/maestro, tag cli-2.12.1),
// maestro-android/src/androidTest/java/dev/mobile/maestro/MaestroDriverService.kt
// (`Service`).
// Changes: only the RPCs Tapwing calls are served (`launchApp` and `addMedia`
// answer UNIMPLEMENTED); `inputText` also accepts non-ASCII text, which it
// types without switching the IME (UnicodeTextInput) instead of dropping it;
// `enableMockLocationProviders` grants this package the mock-location app-op
// and location permissions itself, `disableLocationUpdates` hands the app-op
// back, and mock location left behind by a killed driver is removed at start.
// Hierarchy, tap, key, screenshot, window update and location semantics are
// Maestro's.
package dev.tapwing.driver

import android.Manifest
import android.app.UiAutomation
import android.content.Context
import android.location.Location
import android.location.LocationManager
import android.os.Build
import android.os.SystemClock
import android.util.DisplayMetrics
import android.util.Log
import android.view.KeyEvent
import android.view.WindowManager
import androidx.test.uiautomator.UiDevice
import androidx.test.uiautomator.UiDeviceAccess.clickNoSyncExt
import dev.tapwing.driver.hierarchy.ToastListener
import dev.tapwing.driver.hierarchy.ViewHierarchy
import dev.tapwing.driver.input.AsciiKeys
import dev.tapwing.driver.input.InputInjector
import dev.tapwing.driver.input.UnicodeTextInput
import dev.tapwing.driver.location.MockLocationProvider
import dev.tapwing.driver.location.MockProviders
import dev.tapwing.driver.screenshot.Screenshots
import io.grpc.Metadata
import io.grpc.Status
import io.grpc.StatusRuntimeException
import io.grpc.stub.StreamObserver
import maestro_android.MaestroAndroid
import maestro_android.MaestroDriverGrpc
import java.io.ByteArrayOutputStream
import java.util.concurrent.Executors
import java.util.concurrent.ScheduledFuture
import java.util.concurrent.TimeUnit
import kotlin.system.measureTimeMillis

class DriverService(
    /** This package's context: the instrumentation targets itself. */
    private val context: Context,
    private val uiDevice: UiDevice,
    private val uiAutomation: UiAutomation,
) : MaestroDriverGrpc.MaestroDriverImplBase() {

    private val locationLock = Any()
    private var locationFuture: ScheduledFuture<*>? = null
    private val locationExecutor = Executors.newSingleThreadScheduledExecutor { r ->
        Thread(r, "tapwing-mock-location").apply { isDaemon = true }
    }
    private val mockLocationProviderList = mutableListOf<MockLocationProvider>()
    private var staleMockLocationChecked = false
    private val shell = Shell(uiAutomation)
    private val toastListener = ToastListener.start(uiAutomation)
    private val inputInjector = InputInjector(uiDevice, uiAutomation)
    private val unicodeInput = UnicodeTextInput(uiAutomation, shell)

    init {
        // Off the startup path: the host is waiting for the first `deviceInfo`.
        locationExecutor.execute { synchronized(locationLock) { removeStaleMockLocation() } }
    }

    override fun deviceInfo(
        request: MaestroAndroid.DeviceInfoRequest,
        responseObserver: StreamObserver<MaestroAndroid.DeviceInfo>,
    ) = respond(responseObserver) {
        val windowManager = context.getSystemService(Context.WINDOW_SERVICE) as WindowManager
        val displayMetrics = DisplayMetrics()
        @Suppress("DEPRECATION")
        windowManager.defaultDisplay.getRealMetrics(displayMetrics)
        MaestroAndroid.DeviceInfo.newBuilder()
            .setWidthPixels(displayMetrics.widthPixels)
            .setHeightPixels(displayMetrics.heightPixels)
            .build()
    }

    override fun viewHierarchy(
        request: MaestroAndroid.ViewHierarchyRequest,
        responseObserver: StreamObserver<MaestroAndroid.ViewHierarchyResponse>,
    ) = respond(responseObserver) {
        refreshAccessibilityCache()
        val stream = ByteArrayOutputStream()
        val ms = measureTimeMillis {
            val toast = toastListener.getActiveToastAccessibilityNode()
            ViewHierarchy.dump(context, uiDevice, uiAutomation, stream, toast)
        }
        Log.d(TAG, "View hierarchy dumped in $ms ms")
        MaestroAndroid.ViewHierarchyResponse.newBuilder().setHierarchy(stream.toString(Charsets.UTF_8.name())).build()
    }

    /**
     * Drops stale nodes from the in-process accessibility cache: setting the
     * service info clears it before the (deliberately) null info throws.
     * `waitForIdle(500)` waits for up to 500 ms of accessibility quiet first,
     * exactly as Maestro does before every dump.
     */
    private fun refreshAccessibilityCache() {
        try {
            uiDevice.waitForIdle(500)
            uiAutomation.serviceInfo = null
        } catch (_: NullPointerException) {
            // Expected, see above.
        }
    }

    override fun tap(request: MaestroAndroid.TapRequest, responseObserver: StreamObserver<MaestroAndroid.TapResponse>) =
        respond(responseObserver) {
            uiDevice.clickNoSyncExt(request.x, request.y)
            MaestroAndroid.TapResponse.getDefaultInstance()
        }

    override fun eraseAllText(
        request: MaestroAndroid.EraseAllTextRequest,
        responseObserver: StreamObserver<MaestroAndroid.EraseAllTextResponse>,
    ) = respond(responseObserver) {
        repeat(request.charactersToErase) { inputInjector.pressKeyCode(KeyEvent.KEYCODE_DEL) }
        MaestroAndroid.EraseAllTextResponse.getDefaultInstance()
    }

    /**
     * ASCII: one key press per character, 75 ms apart, like Maestro. Anything
     * else: [UnicodeTextInput] (where Maestro's host would switch to its IME).
     */
    override fun inputText(
        request: MaestroAndroid.InputTextRequest,
        responseObserver: StreamObserver<MaestroAndroid.InputTextResponse>,
    ) = respond(responseObserver) {
        val text = request.text
        if (AsciiKeys.isAscii(text)) {
            text.forEach { ch ->
                AsciiKeys.forChar(ch)?.let { inputInjector.pressKeyCode(it.keyCode, it.metaState) }
                Thread.sleep(ASCII_KEY_INTERVAL_MS)
            }
        } else {
            val path = unicodeInput.input(text)
            Log.d(TAG, "Unicode input via $path")
        }
        MaestroAndroid.InputTextResponse.getDefaultInstance()
    }

    override fun screenshot(
        request: MaestroAndroid.ScreenshotRequest,
        responseObserver: StreamObserver<MaestroAndroid.ScreenshotResponse>,
    ) = respond(responseObserver) {
        val bitmap = Screenshots.takeWithRetry { uiAutomation.takeScreenshot() }
        val bytes = try {
            Screenshots.encodePng(bitmap)
        } finally {
            bitmap.recycle()
        }
        MaestroAndroid.ScreenshotResponse.newBuilder().setBytes(bytes).build()
    }

    override fun isWindowUpdating(
        request: MaestroAndroid.CheckWindowUpdatingRequest,
        responseObserver: StreamObserver<MaestroAndroid.CheckWindowUpdatingResponse>,
    ) = respond(responseObserver) {
        MaestroAndroid.CheckWindowUpdatingResponse.newBuilder()
            .setIsWindowUpdating(uiDevice.waitForWindowUpdate(request.appId, WINDOW_UPDATE_TIMEOUT_MS))
            .build()
    }

    override fun enableMockLocationProviders(
        request: MaestroAndroid.EmptyRequest,
        responseObserver: StreamObserver<MaestroAndroid.EmptyResponse>,
    ) = respond(responseObserver) {
        val locationManager = context.getSystemService(Context.LOCATION_SERVICE) as LocationManager
        synchronized(locationLock) {
            removeStaleMockLocation()
            grantMockLocation()
            if (mockLocationProviderList.isEmpty()) {
                mockLocationProviderList.addAll(MockProviders.create(context, locationManager))
            }
            mockLocationProviderList.forEach { it.enable() }
        }
        MaestroAndroid.EmptyResponse.getDefaultInstance()
    }

    /**
     * Undoes the mock location a previous driver process left behind when it
     * was killed (host crash, SIGKILL, a teardown that timed out): test
     * providers are registered with the OS, not with the process that added
     * them, so the device would keep reporting the last fake position until
     * it reboots. Runs once per process, before the first use and shortly
     * after start. Call with [locationLock] held.
     */
    private fun removeStaleMockLocation() {
        if (staleMockLocationChecked) return
        staleMockLocationChecked = true
        val pkg = context.packageName
        // Only a process that got as far as the app-op can have left providers.
        val allowed = runCatching { shell.run("appops get $pkg android:mock_location") }
            .getOrDefault("").contains("allow")
        if (!allowed) return
        val locationManager = context.getSystemService(Context.LOCATION_SERVICE) as LocationManager
        runCatching { MockProviders.create(context, locationManager) }.getOrDefault(emptyList()).forEach { provider ->
            // Throws for a provider that is not mocked: nothing to undo there.
            runCatching { provider.disable() }
        }
        revokeMockLocation()
        Log.i(TAG, "Removed the mock location a previous driver left behind")
    }

    /** Hands the mock-location app-op back once no test provider needs it. */
    private fun revokeMockLocation() {
        runCatching { shell.run("appops set ${context.packageName} android:mock_location default") }
    }

    /** What Maestro's host does before `enableMockLocationProviders`, for this package. */
    private fun grantMockLocation() {
        val pkg = context.packageName
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.P) {
            uiAutomation.grantRuntimePermission(pkg, Manifest.permission.ACCESS_FINE_LOCATION)
            uiAutomation.grantRuntimePermission(pkg, Manifest.permission.ACCESS_COARSE_LOCATION)
        } else {
            shell.run("pm grant $pkg ${Manifest.permission.ACCESS_FINE_LOCATION}")
            shell.run("pm grant $pkg ${Manifest.permission.ACCESS_COARSE_LOCATION}")
        }
        shell.run("appops set $pkg android:mock_location allow")
    }

    override fun disableLocationUpdates(
        request: MaestroAndroid.EmptyRequest,
        responseObserver: StreamObserver<MaestroAndroid.EmptyResponse>,
    ) = respond(responseObserver) {
        synchronized(locationLock) {
            locationFuture?.cancel(false)
            locationFuture = null
            val hadProviders = mockLocationProviderList.isNotEmpty()
            mockLocationProviderList.forEach { provider ->
                runCatching { provider.disable() }
            }
            mockLocationProviderList.clear()
            if (hadProviders) revokeMockLocation()
        }
        MaestroAndroid.EmptyResponse.getDefaultInstance()
    }

    /** Re-sends the location every 2 s to every mock provider until replaced. */
    override fun setLocation(
        request: MaestroAndroid.SetLocationRequest,
        responseObserver: StreamObserver<MaestroAndroid.SetLocationResponse>,
    ) = respond(responseObserver) {
        synchronized(locationLock) {
            locationFuture?.cancel(false)
            val lat = request.latitude
            val lon = request.longitude
            locationFuture = locationExecutor.scheduleAtFixedRate(
                {
                    val providers = synchronized(locationLock) { mockLocationProviderList.toList() }
                    providers.forEach { provider ->
                        runCatching {
                            val location = Location(provider.getProviderName()).apply {
                                latitude = lat
                                longitude = lon
                                // `Criteria.ACCURACY_FINE` (deprecated): Maestro reports accuracy 1 m.
                                accuracy = 1f
                                altitude = 0.0
                                time = System.currentTimeMillis()
                                elapsedRealtimeNanos = SystemClock.elapsedRealtimeNanos()
                            }
                            provider.setLocation(location)
                        }.onFailure { err ->
                            Log.w(TAG, "Mock location tick failed for ${provider.getProviderName()}", err)
                        }
                    }
                },
                0L,
                LOCATION_UPDATE_INTERVAL_MS,
                TimeUnit.MILLISECONDS,
            )
        }
        MaestroAndroid.SetLocationResponse.getDefaultInstance()
    }

    /** Runs [body] and completes the call, mapping any throwable like Maestro's `internalError`. */
    private inline fun <T> respond(observer: StreamObserver<T>, body: () -> T) {
        val response = try {
            body()
        } catch (t: Throwable) {
            Log.e(TAG, "RPC failed", t)
            observer.onError(t.internalError())
            return
        }
        observer.onNext(response)
        observer.onCompleted()
    }

    private fun Throwable.internalError(): StatusRuntimeException {
        val trailers = Metadata().apply {
            put(ERROR_TYPE_KEY, this@internalError::class.java.name)
            this@internalError.message?.let { put(ERROR_MSG_KEY, it.asciiOnly()) }
            this@internalError.cause?.let { put(ERROR_CAUSE_KEY, it.toString().asciiOnly()) }
        }
        return Status.INTERNAL.withDescription(message).asRuntimeException(trailers)
    }

    /** ASCII metadata values must be printable ASCII; gRPC rejects the whole trailer otherwise. */
    private fun String.asciiOnly(): String = map { if (it.code in 0x20..0x7E) it else '?' }.joinToString("")

    private companion object {
        const val TAG = "TapwingDriver"
        const val ASCII_KEY_INTERVAL_MS = 75L
        const val WINDOW_UPDATE_TIMEOUT_MS = 500L
        const val LOCATION_UPDATE_INTERVAL_MS = 2000L

        val ERROR_TYPE_KEY: Metadata.Key<String> = Metadata.Key.of("error-type", Metadata.ASCII_STRING_MARSHALLER)
        val ERROR_MSG_KEY: Metadata.Key<String> = Metadata.Key.of("error-message", Metadata.ASCII_STRING_MARSHALLER)
        val ERROR_CAUSE_KEY: Metadata.Key<String> = Metadata.Key.of("error-cause", Metadata.ASCII_STRING_MARSHALLER)
    }
}

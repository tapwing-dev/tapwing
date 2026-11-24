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
// (`MaestroDriverService.grpcServer`).
// Changes: a plain `Instrumentation` targeting its own package replaces the
// JUnit test run by `AndroidJUnitRunner` in a second (test) APK; the server is
// grpc-okhttp instead of grpc-netty-shaded (same HTTP/2 wire, a much smaller APK);
// it listens on the IPv4 loopback only, not on every interface.
package dev.tapwing.driver

import android.app.Activity
import android.app.Instrumentation
import android.os.Bundle
import android.util.Log
import androidx.test.platform.app.InstrumentationRegistry
import androidx.test.uiautomator.Configurator
import androidx.test.uiautomator.UiDevice
import io.grpc.InsecureServerCredentials
import io.grpc.okhttp.OkHttpServerBuilder
import java.net.InetAddress
import java.net.InetSocketAddress
import java.util.concurrent.TimeUnit

/**
 * `am instrument -w -e port 7001 dev.tapwing.driver/.TapwingDriverInstrumentation`
 * serves `maestro_android.MaestroDriver` on the device port until the
 * instrumentation is killed (`am force-stop dev.tapwing.driver`).
 */
class TapwingDriverInstrumentation : Instrumentation() {

    private var arguments: Bundle = Bundle()

    override fun onCreate(arguments: Bundle?) {
        super.onCreate(arguments)
        this.arguments = arguments ?: Bundle()
        start()
    }

    override fun onStart() {
        super.onStart()
        try {
            serve()
        } catch (t: Throwable) {
            Log.e(TAG, "Driver failed", t)
            try {
                finish(Activity.RESULT_CANCELED, Bundle().apply { putString("error", t.toString()) })
            } catch (finishErr: Throwable) {
                // `Instrumentation.finish()` throws IllegalStateException when
                // UiAutomation failed while connecting (e.g. another UiAutomation
                // client such as a stale Maestro driver still holds the slot).
                Log.e(TAG, "Instrumentation.finish() failed after driver error: $t", finishErr)
            }
        }
    }

    private fun serve() {
        // UiAutomator reads the instrumentation from the registry in places.
        InstrumentationRegistry.registerInstance(this, arguments)
        Configurator.getInstance()
            .setActionAcknowledgmentTimeout(0L)
            .setWaitForIdleTimeout(0L)
            .setWaitForSelectorTimeout(0L)

        // Order matters: UiDevice installs UiAutomator's accessibility event
        // listener, DriverService then replaces it with the toast listener.
        val uiDevice = UiDevice.getInstance(this)
        val uiAutomation = uiAutomation
        val port = arguments.getString("port", DEFAULT_PORT.toString()).toInt()

        // The server has no authentication and UiAutomation rights, so it must
        // not be reachable from the network the device is on. `adb forward`
        // connects to the device's IPv4 loopback; `InetAddress.getLoopbackAddress()`
        // is `::1` on Android and would not be reached.
        val loopback = InetSocketAddress(InetAddress.getByAddress(byteArrayOf(127, 0, 0, 1)), port)
        val server = OkHttpServerBuilder.forPort(loopback, InsecureServerCredentials.create())
            .addService(DriverService(context, uiDevice, uiAutomation))
            // Same keep-alive policy as Maestro's Netty server.
            .permitKeepAliveTime(30, TimeUnit.SECONDS)
            .permitKeepAliveWithoutCalls(true)
            .keepAliveTimeout(20, TimeUnit.SECONDS)
            .maxConnectionIdle(30, TimeUnit.MINUTES)
            .build()
            .start()
        Log.i(TAG, "Tapwing driver listening on 127.0.0.1:$port")
        server.awaitTermination()
    }

    private companion object {
        const val TAG = "TapwingDriver"
        const val DEFAULT_PORT = 7001
    }
}

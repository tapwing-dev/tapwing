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
// Ported from Maestro 2.12.1 (github.com/mobile-dev-inc/maestro, tag cli-2.12.1),
// maestro-android/src/androidTest/java/dev/mobile/maestro/screenshot/ScreenshotService.kt.
// Changes: the kotlin-retry policy (3 attempts, binary exponential backoff from
// 500 ms, only on a null bitmap) is a plain loop; PNG only.
package dev.tapwing.driver.screenshot

import android.graphics.Bitmap
import com.google.protobuf.ByteString
import java.io.ByteArrayOutputStream

class ScreenshotException(message: String, cause: Throwable? = null) : Exception(message, cause)

object Screenshots {

    private const val ATTEMPTS = 3
    private const val INITIAL_BACKOFF_MS = 500L

    /** Retries while the capture returns `null` (window not ready / SurfaceControl invalid). */
    fun takeWithRetry(capture: () -> Bitmap?): Bitmap {
        var backoff = INITIAL_BACKOFF_MS
        repeat(ATTEMPTS - 1) {
            capture()?.let { return it }
            Thread.sleep(backoff)
            backoff *= 2
        }
        return capture() ?: throw NullPointerException("Screenshot returned null — window may not be ready")
    }

    fun encodePng(bitmap: Bitmap): ByteString {
        val outputStream = ByteArrayOutputStream()
        val ok = try {
            bitmap.compress(Bitmap.CompressFormat.PNG, 100, outputStream)
        } catch (t: Throwable) {
            throw ScreenshotException(
                "Bitmap compression failed: format=PNG, width=${bitmap.width}, height=${bitmap.height}, config=${bitmap.config}",
                t,
            )
        }
        if (!ok) {
            throw ScreenshotException(
                "Bitmap.compress returned false: format=PNG, width=${bitmap.width}, height=${bitmap.height}, config=${bitmap.config}",
            )
        }
        val bytes = outputStream.toByteArray()
        if (bytes.isEmpty()) {
            throw ScreenshotException(
                "Bitmap compressed but produced empty output: format=PNG, width=${bitmap.width}, height=${bitmap.height}",
            )
        }
        return ByteString.copyFrom(bytes)
    }
}

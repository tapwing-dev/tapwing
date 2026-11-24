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
// maestro-android/src/androidTest/java/dev/mobile/maestro/InputInjector.kt.
// Changes: package name only.
package dev.tapwing.driver.input

import android.app.UiAutomation
import android.os.SystemClock
import android.util.Log
import android.view.InputDevice
import android.view.InputEvent
import android.view.KeyCharacterMap
import android.view.KeyEvent
import androidx.test.uiautomator.UiDevice
import java.lang.reflect.InvocationTargetException
import java.lang.reflect.Method

/**
 * Presses keys without waiting for window animations. UiAutomator's injection
 * waits up to 5 s before and after each key for animations to end, and an
 * animation Android leaves running (a `starting_reveal` after a slow splash
 * handoff) makes `inputText` exceed the client's deadline. Uses the hidden
 * `injectInputEvent(event, sync, waitForAnimations)` from API 31 (instrumented
 * processes may call test APIs), else UiAutomator.
 */
internal class InputInjector(
    private val uiDevice: UiDevice,
    private val uiAutomation: UiAutomation,
) {

    // Null before API 31.
    private val injectInputEventMethod: Method? = runCatching {
        UiAutomation::class.java.getMethod(
            "injectInputEvent",
            InputEvent::class.java,
            Boolean::class.javaPrimitiveType,
            Boolean::class.javaPrimitiveType,
        )
    }.onFailure {
        Log.i(TAG, "injectInputEvent(event, sync, waitForAnimations) unavailable, using UiAutomator: $it")
    }.getOrNull()

    fun pressKeyCode(keyCode: Int, metaState: Int = 0): Boolean {
        val method = injectInputEventMethod ?: return uiDevice.pressKeyCode(keyCode, metaState)

        // Same events UiAutomator's InteractionController.sendKey builds.
        val eventTime = SystemClock.uptimeMillis()
        return send(method, keyEvent(eventTime, KeyEvent.ACTION_DOWN, keyCode, metaState)) &&
            send(method, keyEvent(eventTime, KeyEvent.ACTION_UP, keyCode, metaState))
    }

    private fun send(method: Method, event: InputEvent): Boolean {
        try {
            return method.invoke(uiAutomation, event, /* sync = */ true, /* waitForAnimations = */ false) as Boolean
        } catch (e: InvocationTargetException) {
            // Surface the real failure (for example a SecurityException) the way a direct call would.
            throw e.cause ?: e
        }
    }

    private fun keyEvent(eventTime: Long, action: Int, keyCode: Int, metaState: Int): KeyEvent {
        return KeyEvent(
            eventTime, eventTime, action, keyCode, 0, metaState,
            KeyCharacterMap.VIRTUAL_KEYBOARD, 0, 0, InputDevice.SOURCE_KEYBOARD,
        )
    }

    private companion object {
        private const val TAG = "TapwingDriver"
    }
}

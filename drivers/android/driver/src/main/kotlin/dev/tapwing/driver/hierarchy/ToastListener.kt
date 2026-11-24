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
// maestro-android/src/androidTest/java/dev/mobile/maestro/ToastAccessibilityListener.kt.
// Changes: `Toast::class.java.name` instead of `jvmName` (no kotlin-reflect),
// `@Volatile` state (events arrive on the main looper, reads on gRPC threads).
package dev.tapwing.driver.hierarchy

import android.app.UiAutomation
import android.os.Build
import android.util.Log
import android.view.accessibility.AccessibilityEvent
import android.view.accessibility.AccessibilityNodeInfo
import android.widget.Toast

/**
 * Remembers the last toast: toasts are not part of any window's accessibility
 * tree, so the hierarchy dump appends a synthetic node for one shown within
 * the last 3.5 s (`Toast.LENGTH_LONG`).
 */
object ToastListener : UiAutomation.OnAccessibilityEventListener {

    private data class ToastSnapshot(
        val node: AccessibilityNodeInfo,
        val timestampMillis: Long,
    )

    @Volatile private var recentToast: ToastSnapshot? = null
    @Volatile private var isListening = false

    private const val TOAST_LENGTH_LONG_DURATION = 3500

    @Suppress("DEPRECATION")
    override fun onAccessibilityEvent(accessibilityEvent: AccessibilityEvent) {
        if (
            accessibilityEvent.eventType == AccessibilityEvent.TYPE_NOTIFICATION_STATE_CHANGED &&
            accessibilityEvent.className.toString().contains(Toast::class.java.name)
        ) {
            // The public constructor only exists from API 30.
            val nodeInfo = if (Build.VERSION.SDK_INT < 30) {
                @Suppress("DEPRECATION")
                AccessibilityNodeInfo.obtain()
            } else {
                AccessibilityNodeInfo()
            }.apply {
                text = accessibilityEvent.text.firstOrNull()?.toString() ?: ""
                className = Toast::class.java.name
                isVisibleToUser = true
                viewIdResourceName = ""
                packageName = ""
                isCheckable = false
                isChecked = accessibilityEvent.isChecked
                isClickable = false
                isEnabled = accessibilityEvent.isEnabled
                Log.d("TapwingDriver", "Toast received with $text")
            }
            recentToast = ToastSnapshot(node = nodeInfo, timestampMillis = System.currentTimeMillis())
        }
    }

    fun getToastAccessibilityNode(): AccessibilityNodeInfo? = recentToast?.node

    fun isTimedOut(): Boolean {
        val snapshot = recentToast ?: return true
        return System.currentTimeMillis() - snapshot.timestampMillis > TOAST_LENGTH_LONG_DURATION
    }

    /**
     * Atomically reads the latest toast node only if it has not timed out,
     * preventing torn reads between node and timestamp across threads.
     */
    fun getActiveToastAccessibilityNode(): AccessibilityNodeInfo? {
        val snapshot = recentToast ?: return null
        return if (System.currentTimeMillis() - snapshot.timestampMillis <= TOAST_LENGTH_LONG_DURATION) {
            snapshot.node
        } else {
            null
        }
    }

    /**
     * Must run after `UiDevice.getInstance`: the listener slot is single, and
     * Maestro installs this one last, replacing UiAutomator's.
     */
    fun start(uiAutomation: UiAutomation): ToastListener {
        if (isListening) return this
        uiAutomation.setOnAccessibilityEventListener(this)
        isListening = true
        return this
    }
}

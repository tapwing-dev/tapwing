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
// maestro-android/src/androidTest/java/androidx/test/uiautomator/UiDeviceExt.kt.
// Changes: also exposes `getWindowRoots()`, which Maestro calls via reflection
// (`ViewHierarchy.kt`); the same package gives compile-time access instead.
package androidx.test.uiautomator

import android.view.accessibility.AccessibilityNodeInfo

/**
 * Package-private UiAutomator 2.3.0 internals Maestro's driver relies on. They
 * live in this package so Kotlin can reach them without reflection.
 */
object UiDeviceAccess {

    /**
     * `UiDevice.click()` discards taps it thinks are off-screen (it computes the
     * screen bounds incorrectly). Maestro taps through the interaction
     * controller directly: touch down, 100 ms, touch up.
     */
    fun UiDevice.clickNoSyncExt(x: Int, y: Int): Boolean = interactionController.clickNoSync(x, y)

    /**
     * The roots of every window UiAutomator sees (active window first in
     * insertion, but collected in a `HashSet`, so the order is the set's).
     * Maestro's hierarchy dump iterates exactly this array.
     */
    fun UiDevice.windowRoots(): Array<AccessibilityNodeInfo> = getWindowRoots()
}

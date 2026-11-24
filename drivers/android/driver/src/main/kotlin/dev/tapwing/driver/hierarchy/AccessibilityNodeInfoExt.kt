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
// maestro-android/src/androidTest/java/dev/mobile/maestro/AccessibilityNodeInfoExt.kt.
// Changes: package name only.
package dev.tapwing.driver.hierarchy

import android.os.Build
import android.view.accessibility.AccessibilityNodeInfo

object AccessibilityNodeInfoExt {

    /** The hint text, or `""` below API 26 or when there is none. */
    fun AccessibilityNodeInfo.getHintOrFallback(): CharSequence {
        return if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O && this.hintText != null) {
            this.hintText
        } else {
            ""
        }
    }

    /**
     * The text, except that an empty input showing its hint (which API 26+
     * reports through `getText`) yields `""`, so the hint only appears as
     * `hintText`.
     */
    fun AccessibilityNodeInfo.getTextOrFallback(): CharSequence {
        return if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O && this.isShowingHintText) {
            ""
        } else {
            this.text ?: ""
        }
    }
}

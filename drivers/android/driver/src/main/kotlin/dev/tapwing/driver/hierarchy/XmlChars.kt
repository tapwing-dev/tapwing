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
// maestro-android/src/androidTest/java/dev/mobile/maestro/ViewHierarchy.kt
// (`safeCharSeqToString`, `stripInvalidXMLChars`).
// Changes: extracted into a pure function so it can be unit-tested on the JVM.
package dev.tapwing.driver.hierarchy

object XmlChars {

    /** `null` becomes `""`; characters XML 1.1 discourages become `.`. */
    fun safe(cs: CharSequence?): String = cs?.let(::stripInvalid) ?: ""

    /**
     * Maestro's check runs per UTF-16 `Char`, so the supplementary-plane ranges
     * it lists (`#x1FFFE`... ) can never match; only the BMP ranges below take
     * effect, and that exact behaviour is what this reproduces. `U+0000` is not
     * in the list and passes through, as in Maestro.
     */
    fun stripInvalid(cs: CharSequence): String {
        val ret = StringBuilder(cs.length)
        for (i in 0 until cs.length) {
            val ch = cs[i]
            ret.append(if (isReplaced(ch.code)) '.' else ch)
        }
        return ret.toString()
    }

    private fun isReplaced(code: Int): Boolean =
        code in 0x1..0x8 || code in 0xB..0xC || code in 0xE..0x1F ||
            code in 0x7F..0x84 || code in 0x86..0x9F || code in 0xFDD0..0xFDDF
}

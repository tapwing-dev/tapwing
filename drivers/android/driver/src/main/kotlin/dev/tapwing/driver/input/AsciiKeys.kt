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
// (`Service.setText`, `keyPressShiftedToEvents`).
// Changes: extracted into a pure character -> key press table so the mapping
// is unit-tested on the JVM; the keys and modifiers are unchanged.
package dev.tapwing.driver.input

import android.view.KeyEvent

/** One key press of Maestro's ASCII `inputText`. */
data class KeyPress(val keyCode: Int, val metaState: Int = 0)

object AsciiKeys {

    /**
     * The key Maestro presses for [ch], or `null` for characters it silently
     * skips (control characters such as `\n` and `\t`, and anything non-ASCII).
     * Letters use raw key codes (upper case with meta state `1`, as Maestro
     * does), `( ) +` use the numeric keypad keys.
     */
    fun forChar(ch: Char): KeyPress? {
        val code = ch.code
        return when (code) {
            in 48..57 -> KeyPress(code - 41) // 0-9
            in 65..90 -> KeyPress(code - 36, 1) // A-Z
            in 97..122 -> KeyPress(code - 68) // a-z
            ';'.code -> KeyPress(KeyEvent.KEYCODE_SEMICOLON)
            '='.code -> KeyPress(KeyEvent.KEYCODE_EQUALS)
            ','.code -> KeyPress(KeyEvent.KEYCODE_COMMA)
            '-'.code -> KeyPress(KeyEvent.KEYCODE_MINUS)
            '.'.code -> KeyPress(KeyEvent.KEYCODE_PERIOD)
            '/'.code -> KeyPress(KeyEvent.KEYCODE_SLASH)
            '`'.code -> KeyPress(KeyEvent.KEYCODE_GRAVE)
            '\''.code -> KeyPress(KeyEvent.KEYCODE_APOSTROPHE)
            '['.code -> KeyPress(KeyEvent.KEYCODE_LEFT_BRACKET)
            ']'.code -> KeyPress(KeyEvent.KEYCODE_RIGHT_BRACKET)
            '\\'.code -> KeyPress(KeyEvent.KEYCODE_BACKSLASH)
            ' '.code -> KeyPress(KeyEvent.KEYCODE_SPACE)
            '@'.code -> KeyPress(KeyEvent.KEYCODE_AT)
            '#'.code -> KeyPress(KeyEvent.KEYCODE_POUND)
            '*'.code -> KeyPress(KeyEvent.KEYCODE_STAR)
            '('.code -> KeyPress(KeyEvent.KEYCODE_NUMPAD_LEFT_PAREN)
            ')'.code -> KeyPress(KeyEvent.KEYCODE_NUMPAD_RIGHT_PAREN)
            '+'.code -> KeyPress(KeyEvent.KEYCODE_NUMPAD_ADD)
            '!'.code -> shifted(KeyEvent.KEYCODE_1)
            '$'.code -> shifted(KeyEvent.KEYCODE_4)
            '%'.code -> shifted(KeyEvent.KEYCODE_5)
            '^'.code -> shifted(KeyEvent.KEYCODE_6)
            '&'.code -> shifted(KeyEvent.KEYCODE_7)
            '"'.code -> shifted(KeyEvent.KEYCODE_APOSTROPHE)
            '{'.code -> shifted(KeyEvent.KEYCODE_LEFT_BRACKET)
            '}'.code -> shifted(KeyEvent.KEYCODE_RIGHT_BRACKET)
            ':'.code -> shifted(KeyEvent.KEYCODE_SEMICOLON)
            '|'.code -> shifted(KeyEvent.KEYCODE_BACKSLASH)
            '<'.code -> shifted(KeyEvent.KEYCODE_COMMA)
            '>'.code -> shifted(KeyEvent.KEYCODE_PERIOD)
            '?'.code -> shifted(KeyEvent.KEYCODE_SLASH)
            '~'.code -> shifted(KeyEvent.KEYCODE_GRAVE)
            '_'.code -> shifted(KeyEvent.KEYCODE_MINUS)
            else -> null
        }
    }

    private fun shifted(keyCode: Int) = KeyPress(keyCode, KeyEvent.META_SHIFT_LEFT_ON)

    /** Maestro's host only sends text through key presses when it is pure US-ASCII. */
    fun isAscii(text: String): Boolean = text.all { it.code < 0x80 }
}

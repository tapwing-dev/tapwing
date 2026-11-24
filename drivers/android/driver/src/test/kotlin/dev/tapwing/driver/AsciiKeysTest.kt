package dev.tapwing.driver

import android.view.KeyEvent
import dev.tapwing.driver.input.AsciiKeys
import dev.tapwing.driver.input.KeyPress
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class AsciiKeysTest {

    @Test
    fun lettersAndDigitsUseRawKeyCodes() {
        assertEquals(KeyPress(KeyEvent.KEYCODE_0), AsciiKeys.forChar('0'))
        assertEquals(KeyPress(KeyEvent.KEYCODE_9), AsciiKeys.forChar('9'))
        assertEquals(KeyPress(KeyEvent.KEYCODE_A), AsciiKeys.forChar('a'))
        assertEquals(KeyPress(KeyEvent.KEYCODE_Z), AsciiKeys.forChar('z'))
        // Upper case: same key, meta state 1 (META_SHIFT_ON), as in Maestro.
        assertEquals(KeyPress(KeyEvent.KEYCODE_A, 1), AsciiKeys.forChar('A'))
        assertEquals(KeyPress(KeyEvent.KEYCODE_Z, 1), AsciiKeys.forChar('Z'))
    }

    @Test
    fun punctuationMatchesMaestro() {
        assertEquals(KeyPress(KeyEvent.KEYCODE_AT), AsciiKeys.forChar('@'))
        assertEquals(KeyPress(KeyEvent.KEYCODE_NUMPAD_ADD), AsciiKeys.forChar('+'))
        assertEquals(KeyPress(KeyEvent.KEYCODE_NUMPAD_LEFT_PAREN), AsciiKeys.forChar('('))
        assertEquals(KeyPress(KeyEvent.KEYCODE_1, KeyEvent.META_SHIFT_LEFT_ON), AsciiKeys.forChar('!'))
        assertEquals(KeyPress(KeyEvent.KEYCODE_MINUS, KeyEvent.META_SHIFT_LEFT_ON), AsciiKeys.forChar('_'))
        assertEquals(KeyPress(KeyEvent.KEYCODE_APOSTROPHE, KeyEvent.META_SHIFT_LEFT_ON), AsciiKeys.forChar('"'))
    }

    @Test
    fun everyPrintableAsciiCharacterHasAKey() {
        for (c in ' '..'~') {
            assertTrue("no key for '$c'", AsciiKeys.forChar(c) != null)
        }
    }

    @Test
    fun controlCharactersAreSkippedLikeMaestro() {
        assertNull(AsciiKeys.forChar('\n'))
        assertNull(AsciiKeys.forChar('\t'))
        assertNull(AsciiKeys.forChar('ž'))
    }

    @Test
    fun asciiDetectionMatchesTheUsAsciiEncoder() {
        assertTrue(AsciiKeys.isAscii("hello 42\n~"))
        assertTrue(AsciiKeys.isAscii(""))
        assertFalse(AsciiKeys.isAscii("Příliš"))
        assertFalse(AsciiKeys.isAscii("\u0080"))
    }

    @Test
    fun digitsMapToConsecutiveKeyCodesWithoutModifiers() {
        for ((i, c) in ('0'..'9').withIndex()) {
            assertEquals("'$c'", KeyPress(KeyEvent.KEYCODE_0 + i), AsciiKeys.forChar(c))
        }
    }

    @Test
    fun lettersMapToConsecutiveKeyCodesAndUpperCaseAddsShift() {
        for ((i, c) in ('a'..'z').withIndex()) {
            assertEquals("'$c'", KeyPress(KeyEvent.KEYCODE_A + i), AsciiKeys.forChar(c))
            assertEquals("'${c.uppercaseChar()}'", KeyPress(KeyEvent.KEYCODE_A + i, 1), AsciiKeys.forChar(c.uppercaseChar()))
        }
    }

    @Test
    fun unshiftedPunctuationUsesItsOwnKey() {
        val expected = mapOf(
            ';' to KeyEvent.KEYCODE_SEMICOLON,
            '=' to KeyEvent.KEYCODE_EQUALS,
            ',' to KeyEvent.KEYCODE_COMMA,
            '-' to KeyEvent.KEYCODE_MINUS,
            '.' to KeyEvent.KEYCODE_PERIOD,
            '/' to KeyEvent.KEYCODE_SLASH,
            '`' to KeyEvent.KEYCODE_GRAVE,
            '\'' to KeyEvent.KEYCODE_APOSTROPHE,
            '[' to KeyEvent.KEYCODE_LEFT_BRACKET,
            ']' to KeyEvent.KEYCODE_RIGHT_BRACKET,
            '\\' to KeyEvent.KEYCODE_BACKSLASH,
            ' ' to KeyEvent.KEYCODE_SPACE,
            '@' to KeyEvent.KEYCODE_AT,
            '#' to KeyEvent.KEYCODE_POUND,
            '*' to KeyEvent.KEYCODE_STAR,
        )
        for ((c, code) in expected) {
            assertEquals("'$c'", KeyPress(code), AsciiKeys.forChar(c))
        }
    }

    @Test
    fun shiftedPunctuationUsesTheBaseKeyWithLeftShift() {
        val expected = mapOf(
            '!' to KeyEvent.KEYCODE_1,
            '$' to KeyEvent.KEYCODE_4,
            '%' to KeyEvent.KEYCODE_5,
            '^' to KeyEvent.KEYCODE_6,
            '&' to KeyEvent.KEYCODE_7,
            '"' to KeyEvent.KEYCODE_APOSTROPHE,
            '{' to KeyEvent.KEYCODE_LEFT_BRACKET,
            '}' to KeyEvent.KEYCODE_RIGHT_BRACKET,
            ':' to KeyEvent.KEYCODE_SEMICOLON,
            '|' to KeyEvent.KEYCODE_BACKSLASH,
            '<' to KeyEvent.KEYCODE_COMMA,
            '>' to KeyEvent.KEYCODE_PERIOD,
            '?' to KeyEvent.KEYCODE_SLASH,
            '~' to KeyEvent.KEYCODE_GRAVE,
            '_' to KeyEvent.KEYCODE_MINUS,
        )
        for ((c, code) in expected) {
            assertEquals("'$c'", KeyPress(code, KeyEvent.META_SHIFT_LEFT_ON), AsciiKeys.forChar(c))
        }
    }

    @Test
    fun parenthesesAndPlusUseTheNumericKeypad() {
        assertEquals(KeyPress(KeyEvent.KEYCODE_NUMPAD_LEFT_PAREN), AsciiKeys.forChar('('))
        assertEquals(KeyPress(KeyEvent.KEYCODE_NUMPAD_RIGHT_PAREN), AsciiKeys.forChar(')'))
        assertEquals(KeyPress(KeyEvent.KEYCODE_NUMPAD_ADD), AsciiKeys.forChar('+'))
    }

    @Test
    fun noTwoPrintableCharactersShareAKeyPress() {
        val presses = (' '..'~').map { AsciiKeys.forChar(it) }
        assertEquals(95, presses.size)
        assertEquals("every printable ASCII character needs its own (key, meta) pair", 95, presses.toSet().size)
    }

    @Test
    fun theShiftedLetterAndTheShiftedSymbolMetaStatesStayDistinct() {
        // Letters use the raw meta value 1 (as in Maestro), symbols use META_SHIFT_LEFT_ON.
        assertEquals(1, AsciiKeys.forChar('Q')!!.metaState)
        assertEquals(KeyEvent.META_SHIFT_LEFT_ON, AsciiKeys.forChar('?')!!.metaState)
        assertEquals(0, AsciiKeys.forChar('q')!!.metaState)
    }

    @Test
    fun everyControlCharacterAndDeleteIsSkipped() {
        for (code in 0x00..0x1F) assertNull("U+%04X".format(code), AsciiKeys.forChar(code.toChar()))
        assertNull(AsciiKeys.forChar('\u007f'))
    }

    @Test
    fun charactersAdjacentToTheLetterAndDigitRangesAreNotMistakenForThem() {
        // '/' (47) and ':' (58) bracket the digits; '@' (64) and '[' (91) bracket upper case;
        // '`' (96) and '{' (123) bracket lower case.
        assertEquals(KeyPress(KeyEvent.KEYCODE_SLASH), AsciiKeys.forChar('/'))
        assertEquals(KeyPress(KeyEvent.KEYCODE_SEMICOLON, KeyEvent.META_SHIFT_LEFT_ON), AsciiKeys.forChar(':'))
        assertEquals(KeyPress(KeyEvent.KEYCODE_AT), AsciiKeys.forChar('@'))
        assertEquals(KeyPress(KeyEvent.KEYCODE_LEFT_BRACKET), AsciiKeys.forChar('['))
        assertEquals(KeyPress(KeyEvent.KEYCODE_GRAVE), AsciiKeys.forChar('`'))
        assertEquals(KeyPress(KeyEvent.KEYCODE_LEFT_BRACKET, KeyEvent.META_SHIFT_LEFT_ON), AsciiKeys.forChar('{'))
    }

    @Test
    fun nonAsciiCharactersAreSkippedIncludingCzechLettersDigitsLookalikesAndSurrogates() {
        for (c in "ěščřžýáíéúůďťňĚŠČŘŽÝÁÍÉÚŮĎŤŇäöüß€£é") assertNull("'$c'", AsciiKeys.forChar(c))
        assertNull(AsciiKeys.forChar('\uFF10')) // fullwidth 0
        assertNull(AsciiKeys.forChar('\u0660')) // Arabic-Indic 0
        assertNull(AsciiKeys.forChar('\u00A0')) // no-break space is not a space key
        assertNull(AsciiKeys.forChar('\u200D')) // zero-width joiner
        // An emoji arrives as two surrogate chars; neither maps to a key.
        for (c in "🚀") assertNull(AsciiKeys.forChar(c))
    }

    @Test
    fun keyPressIsAValueWithZeroMetaStateByDefault() {
        assertEquals(0, KeyPress(KeyEvent.KEYCODE_A).metaState)
        assertEquals(KeyPress(KeyEvent.KEYCODE_A, 0), KeyPress(KeyEvent.KEYCODE_A))
        assertEquals(KeyPress(KeyEvent.KEYCODE_B, 1), KeyPress(KeyEvent.KEYCODE_A, 1).copy(keyCode = KeyEvent.KEYCODE_B))
    }

    @Test
    fun asciiDetectionBoundariesAndLookalikes() {
        assertTrue(AsciiKeys.isAscii("\u0000"))
        assertTrue(AsciiKeys.isAscii("\u007f"))
        assertFalse(AsciiKeys.isAscii("\u0080"))
        assertFalse(AsciiKeys.isAscii("a\u00A0b")) // no-break space
        assertFalse(AsciiKeys.isAscii("e\u0301")) // combining accent
        assertFalse(AsciiKeys.isAscii("rocket 🚀"))
        assertFalse(AsciiKeys.isAscii("\uFF21")) // fullwidth A
        assertTrue(AsciiKeys.isAscii(" !\"#$%&'()*+,-./0123456789:;<=>?@AZ[\\]^_`az{|}~"))
        // A single non-ASCII character anywhere makes the whole text non-ASCII.
        assertFalse(AsciiKeys.isAscii("a".repeat(5000) + "ž"))
    }
}

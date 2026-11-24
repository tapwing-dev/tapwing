package dev.tapwing.driver

import dev.tapwing.driver.hierarchy.XmlChars
import org.junit.Assert.assertEquals
import org.junit.Test

class XmlCharsTest {

    @Test
    fun replacesDiscouragedCharactersWithADot() {
        assertEquals("a.b.c", XmlChars.safe("a\u0001b\u001fc"))
        assertEquals("..", XmlChars.safe("\u007f\u0090"))
        assertEquals(".", XmlChars.safe("﷐"))
    }

    @Test
    fun keepsTextNewlinesNulAndSupplementaryCharacters() {
        assertEquals("Příliš\n\t\r🚀\u0085", XmlChars.safe("Příliš\n\t\r🚀\u0085"))
        // Maestro's list does not include U+0000.
        assertEquals("\u0000", XmlChars.safe("\u0000"))
        assertEquals("", XmlChars.safe(null))
    }

    private fun replaced(code: Int) = XmlChars.safe(code.toChar().toString()) == "."

    @Test
    fun controlCharacterBoundaries() {
        // 0x01..0x08 replaced, 0x09 (tab) and 0x0A (newline) kept, 0x0B/0x0C replaced,
        // 0x0D kept, 0x0E..0x1F replaced, 0x20 (space) kept.
        for (code in 0x01..0x08) assertEquals("U+%04X".format(code), true, replaced(code))
        for (code in listOf(0x09, 0x0A, 0x0D, 0x20)) assertEquals("U+%04X".format(code), false, replaced(code))
        for (code in listOf(0x0B, 0x0C)) assertEquals("U+%04X".format(code), true, replaced(code))
        for (code in 0x0E..0x1F) assertEquals("U+%04X".format(code), true, replaced(code))
    }

    @Test
    fun deleteAndC1ControlBoundaries() {
        assertEquals(false, replaced(0x7E)) // ~
        for (code in 0x7F..0x84) assertEquals("U+%04X".format(code), true, replaced(code))
        // U+0085 NEL is a legal XML 1.1 line break and is kept.
        assertEquals(false, replaced(0x85))
        for (code in 0x86..0x9F) assertEquals("U+%04X".format(code), true, replaced(code))
        assertEquals(false, replaced(0xA0)) // no-break space
        assertEquals(false, replaced(0xA1))
    }

    @Test
    fun nonCharacterBlockBoundaries() {
        assertEquals(false, replaced(0xFDCF))
        for (code in 0xFDD0..0xFDDF) assertEquals("U+%04X".format(code), true, replaced(code))
        assertEquals(false, replaced(0xFDE0))
        // Maestro's per-Char check never reaches U+FFFE / U+FFFF: they pass through untouched.
        assertEquals(false, replaced(0xFFFE))
        assertEquals(false, replaced(0xFFFF))
    }

    @Test
    fun xmlSignificantCharactersAreLeftForTheSerializerToEscape() {
        val text = "<a href=\"x\">Tom & 'Jerry'</a>"
        assertEquals(text, XmlChars.safe(text))
    }

    @Test
    fun czechDiacriticsAndOtherScriptsAreUntouched() {
        val text = "Příliš žluťoučký kůň úpěl ďábelské ódy. Ünïcödé ñ ß Ω 日本語 العربية"
        assertEquals(text, XmlChars.safe(text))
    }

    @Test
    fun emojiAndSurrogatePairsSurviveIncludingLoneSurrogates() {
        val family = "👨‍👩‍👧" // contains ZWJ (U+200D)
        assertEquals(family, XmlChars.safe(family))
        assertEquals("🚀", XmlChars.safe("🚀"))
        // The replacement works per UTF-16 unit and never touches surrogates, paired or not.
        assertEquals("\uD83D", XmlChars.safe("\uD83D"))
        assertEquals("\uDE80x", XmlChars.safe("\uDE80x"))
    }

    @Test
    fun replacementIsOneToOneSoLengthIsPreserved() {
        val text = "a\u0001b\u007fc\u0090d🚀\u001f"
        val safe = XmlChars.safe(text)
        assertEquals(text.length, safe.length)
        assertEquals("a.b.c.d🚀.", safe)
    }

    @Test
    fun emptyInputs() {
        assertEquals("", XmlChars.safe(""))
        assertEquals("", XmlChars.safe(null))
        assertEquals("", XmlChars.stripInvalid(""))
        assertEquals("", XmlChars.safe(StringBuilder()))
    }

    @Test
    fun anyCharSequenceIsAccepted() {
        assertEquals("a.b", XmlChars.safe(StringBuilder("a\u0002b")))
        assertEquals("a.b", XmlChars.stripInvalid(java.nio.CharBuffer.wrap("a\u0002b")))
        val clean = "already clean"
        assertEquals(clean, XmlChars.safe(clean))
        // The result is always a new, independent String.
        assertEquals(clean, XmlChars.stripInvalid(clean))
    }

    @Test
    fun aLongMixedStringKeepsEverythingButTheDiscouragedUnits() {
        val builder = StringBuilder()
        val expected = StringBuilder()
        for (code in 0..0xFFFF) {
            if (code in 0xD800..0xDFFF) continue // surrogates are covered separately
            val ch = code.toChar()
            builder.append(ch)
            val dropped = code in 0x1..0x8 || code in 0xB..0xC || code in 0xE..0x1F ||
                code in 0x7F..0x84 || code in 0x86..0x9F || code in 0xFDD0..0xFDDF
            expected.append(if (dropped) '.' else ch)
        }
        assertEquals(expected.toString(), XmlChars.safe(builder))
    }
}

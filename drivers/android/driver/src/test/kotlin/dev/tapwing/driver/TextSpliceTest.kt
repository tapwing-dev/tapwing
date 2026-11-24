package dev.tapwing.driver

import dev.tapwing.driver.input.TextSplice
import org.junit.Assert.assertEquals
import org.junit.Test

class TextSpliceTest {

    @Test
    fun insertsAtTheCursorAndMovesItAfterTheText() {
        assertEquals(TextSplice("Příliš", 6), TextSplice.insert("", 0, 0, "Příliš"))
        assertEquals(TextSplice("ab žluť", 7), TextSplice.insert("ab ", 3, 3, "žluť"))
        assertEquals(TextSplice("aČb", 2), TextSplice.insert("ab", 1, 1, "Č"))
    }

    @Test
    fun replacesTheSelectionInEitherDirection() {
        assertEquals(TextSplice("a🚀d", 3), TextSplice.insert("abcd", 1, 3, "🚀"))
        assertEquals(TextSplice("a🚀d", 3), TextSplice.insert("abcd", 3, 1, "🚀"))
    }

    @Test
    fun unknownOrStaleSelectionAppends() {
        assertEquals(TextSplice("abč", 3), TextSplice.insert("ab", -1, -1, "č"))
        assertEquals(TextSplice("abč", 3), TextSplice.insert("ab", 5, 5, "č"))
        assertEquals(TextSplice("abč", 3), TextSplice.insert("ab", 0, 9, "č"))
    }

    @Test
    fun aSelectionWithOnlyOneUnknownEndIsTreatedAsUnknown() {
        assertEquals(TextSplice("abč", 3), TextSplice.insert("ab", -1, 1, "č"))
        assertEquals(TextSplice("abč", 3), TextSplice.insert("ab", 1, -1, "č"))
        assertEquals("start past the end is stale", TextSplice("abč", 3), TextSplice.insert("ab", 3, 1, "č"))
    }

    @Test
    fun cursorAtEitherEndOfTheText() {
        assertEquals(TextSplice("Xabc", 1), TextSplice.insert("abc", 0, 0, "X"))
        assertEquals(TextSplice("abcX", 4), TextSplice.insert("abc", 3, 3, "X"))
    }

    @Test
    fun selectAllIsReplacedEntirely() {
        assertEquals(TextSplice("x", 1), TextSplice.insert("abc", 0, 3, "x"))
        assertEquals(TextSplice("x", 1), TextSplice.insert("abc", 3, 0, "x"))
        assertEquals(TextSplice("😀", 2), TextSplice.insert("Příliš žluťoučký", 0, 16, "😀"))
    }

    @Test
    fun insertingNothingDeletesTheSelectionAndKeepsTheCursor() {
        assertEquals(TextSplice("abc", 1), TextSplice.insert("abc", 1, 1, ""))
        assertEquals(TextSplice("ad", 1), TextSplice.insert("abcd", 1, 3, ""))
        assertEquals(TextSplice("", 0), TextSplice.insert("abc", 0, 3, ""))
    }

    @Test
    fun theCursorCountsUtf16UnitsNotCodePoints() {
        // A rocket is two UTF-16 units, a ZWJ family is eight.
        assertEquals(2, TextSplice.insert("", 0, 0, "🚀").cursor)
        assertEquals(8, TextSplice.insert("", 0, 0, "👨‍👩‍👧").cursor)
        assertEquals(TextSplice("🚀🚀", 4), TextSplice.insert("🚀", 2, 2, "🚀"))
        // Inserting between two emoji (index 2) keeps both intact.
        assertEquals(TextSplice("🚀č🚀", 3), TextSplice.insert("🚀🚀", 2, 2, "č"))
    }

    @Test
    fun combiningDiacriticsAreInsertedAsTypedAndNotNormalized() {
        val decomposed = "é" // é as two units
        assertEquals(TextSplice("aéb", 3), TextSplice.insert("ab", 1, 1, decomposed))
        assertEquals(decomposed.length + 1, TextSplice.insert("a", 1, 1, decomposed).cursor)
    }

    @Test
    fun theDataClassBehavesLikeAValue() {
        assertEquals(TextSplice("a", 1), TextSplice("a", 1))
        assertEquals(TextSplice("a", 2), TextSplice("a", 1).copy(cursor = 2))
        assertEquals("a", TextSplice("a", 1).text)
        assertEquals(1, TextSplice("a", 1).cursor)
    }
}

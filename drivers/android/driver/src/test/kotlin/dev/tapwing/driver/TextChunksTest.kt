package dev.tapwing.driver

import dev.tapwing.driver.input.TextChunks
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Assert.fail
import org.junit.Test

class TextChunksTest {

    @Test
    fun neverSplitsASurrogatePair() {
        assertEquals(listOf("a", "🚀", "b"), TextChunks.split("a🚀b", 2))
        assertEquals(listOf("ab", "c"), TextChunks.split("abc", 2))
        assertEquals(emptyList<String>(), TextChunks.split(""))
    }

    @Test
    fun usesMaestrosChunkSize() {
        val text = "č".repeat(2500)
        assertEquals(listOf(1000, 1000, 500), TextChunks.split(text).map { it.length })
        assertEquals(1000, TextChunks.MAX_CHUNK)
    }

    @Test
    fun textAtOrBelowTheLimitIsASingleChunk() {
        assertEquals(listOf("x"), TextChunks.split("x"))
        assertEquals(listOf("a".repeat(1000)), TextChunks.split("a".repeat(1000)))
        assertEquals(listOf(1000, 1), TextChunks.split("a".repeat(1001)).map { it.length })
    }

    @Test
    fun anExactMultipleHasNoTrailingEmptyChunk() {
        assertEquals(listOf("ab", "cd"), TextChunks.split("abcd", 2))
        assertEquals(listOf(1000, 1000), TextChunks.split("é".repeat(2000)).map { it.length })
    }

    @Test
    fun aPairStraddlingTheDefaultBoundaryMovesToTheNextChunk() {
        // The emoji occupies UTF-16 units 999 and 1000, so the first chunk stops one unit short.
        val text = "a".repeat(999) + "🚀" + "b"
        val chunks = TextChunks.split(text)
        assertEquals(listOf(999, 3), chunks.map { it.length })
        assertEquals("🚀b", chunks[1])
        assertEquals(text, chunks.joinToString(""))
    }

    @Test
    fun aPairEndingExactlyOnTheBoundaryStaysInThatChunk() {
        val text = "a".repeat(998) + "🚀" + "b"
        assertEquals(listOf(1000, 1), TextChunks.split(text).map { it.length })
    }

    @Test
    fun aChunkSizeOfOneKeepsEachPairWhole() {
        // A pair cannot be split and cannot fit in one unit: it is emitted whole instead of
        // producing an empty chunk that never advances.
        assertEquals(listOf("a", "🚀", "b"), TextChunks.split("a🚀b", 1))
        assertEquals(listOf("🚀", "🚀"), TextChunks.split("🚀🚀", 1))
        assertEquals(listOf("č", "e", "́"), TextChunks.split("čé", 1))
    }

    @Test
    fun consecutiveEmojiAreKeptWholeAtEveryChunkSize() {
        val text = "🚀".repeat(7)
        for (size in 1..9) {
            val chunks = TextChunks.split(text, size)
            assertEquals("size $size", text, chunks.joinToString(""))
            assertTrue("size $size", chunks.all { it.isNotEmpty() && it.length % 2 == 0 })
        }
    }

    @Test
    fun anInvalidChunkSizeIsRejectedInsteadOfLooping() {
        for (size in listOf(0, -1, Int.MIN_VALUE)) {
            try {
                TextChunks.split("abc", size)
                fail("chunk size $size must be rejected")
            } catch (expected: IllegalArgumentException) {
                assertTrue(expected.message!!.contains("maxChunkSize"))
            }
        }
        // An empty text is still empty for any valid size, even a huge one.
        assertEquals(emptyList<String>(), TextChunks.split("", Int.MAX_VALUE))
        assertEquals(listOf("abc"), TextChunks.split("abc", Int.MAX_VALUE))
    }

    @Test
    fun chunksAlwaysReassembleToTheOriginalWithoutSeveringPairs() {
        val alphabet = listOf("a", "Ž", "č", "🚀", "👨‍👩‍👧", "😀", "\n", " ", "é")
        val random = java.util.Random(42)
        repeat(200) {
            val text = buildString { repeat(random.nextInt(60)) { append(alphabet[random.nextInt(alphabet.size)]) } }
            for (size in listOf(1, 2, 3, 5, 8, 13)) {
                val chunks = TextChunks.split(text, size)
                assertEquals("size $size of '$text'", text, chunks.joinToString(""))
                for (chunk in chunks) {
                    assertTrue("empty chunk (size $size)", chunk.isNotEmpty())
                    assertFalse(
                        "chunk ends with a high surrogate (size $size): '$chunk'",
                        Character.isHighSurrogate(chunk.last()),
                    )
                    assertFalse(
                        "chunk starts with a low surrogate (size $size): '$chunk'",
                        Character.isLowSurrogate(chunk.first()),
                    )
                    // Only a lone pair may overshoot, and only when the size is 1.
                    assertTrue("chunk too long (size $size): '$chunk'", chunk.length <= maxOf(size, 2))
                }
            }
        }
    }

    @Test
    fun loneSurrogatesArePreservedAndDoNotStopProgress() {
        val lonelyHigh = "ab\uD83D"
        val lonelyLow = "\uDE80cd"
        assertEquals(lonelyHigh, TextChunks.split(lonelyHigh, 2).joinToString(""))
        assertEquals(lonelyLow, TextChunks.split(lonelyLow, 2).joinToString(""))
        assertEquals("\uD83D🚀", TextChunks.split("\uD83D🚀", 2).joinToString(""))
    }
}

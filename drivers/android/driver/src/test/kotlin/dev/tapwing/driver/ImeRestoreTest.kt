package dev.tapwing.driver

import dev.tapwing.driver.input.ImeRestore
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class ImeRestoreTest {

    private val gboard = "com.google.android.inputmethod.latin/com.android.inputmethod.latin.LatinIME"
    private val tapwing = "dev.tapwing.driver/.input.TapwingInputMethodService"

    @Test
    fun recordsThePreviousKeyboardAndWhetherOursWasEnabled() {
        assertEquals("$gboard,0", ImeRestore.record(gboard, wasEnabled = false))
        assertEquals("$gboard,1", ImeRestore.record(gboard, wasEnabled = true))
    }

    @Test
    fun aMissingOrUnusableKeyboardIdIsRecordedAsNone() {
        assertEquals("-,0", ImeRestore.record(null, wasEnabled = false))
        assertEquals("-,1", ImeRestore.record("  ", wasEnabled = true))
        // The record is one shell word and one comma-separated pair.
        assertEquals("-,0", ImeRestore.record("a b/.Ime", wasEnabled = false))
        assertEquals("-,0", ImeRestore.record("a,b/.Ime", wasEnabled = false))
    }

    @Test
    fun findsAnEnabledKeyboardAmongEntriesWithSubtypes() {
        val enabled = "$gboard;1594443099:com.google.android.tts/.VoiceIme:$tapwing\n"
        assertTrue(ImeRestore.isEnabled(enabled, tapwing))
        assertTrue(ImeRestore.isEnabled(enabled, gboard))
        assertTrue(ImeRestore.isEnabled("$tapwing;-1:$gboard", tapwing))
        assertFalse(ImeRestore.isEnabled("$gboard;1594443099", tapwing))
        assertFalse(ImeRestore.isEnabled("null", tapwing))
        assertFalse(ImeRestore.isEnabled("", tapwing))
    }
}

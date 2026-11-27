package dev.tapwing.driver

import dev.tapwing.driver.input.ImeRestore
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
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

    @Test
    fun normalizesPreviousDefaultImeOutput() {
        assertEquals(gboard, ImeRestore.normalizePreviousIme(" $gboard\n", tapwing))
        assertNull(ImeRestore.normalizePreviousIme("", tapwing))
        assertNull(ImeRestore.normalizePreviousIme("   \n", tapwing))
        assertNull(ImeRestore.normalizePreviousIme("null\n", tapwing))
        assertNull(ImeRestore.normalizePreviousIme(" $tapwing ", tapwing))
    }

    @Test
    fun buildsOrderedRestoreCommandsForEveryCombination() {
        assertEquals(
            listOf(
                "ime set $gboard",
                "ime disable $tapwing",
                "settings delete global ${ImeRestore.SETTING_KEY}",
            ),
            ImeRestore.restoreCommands(originalIme = gboard, wasEnabled = false, tapwingImeId = tapwing),
        )
        assertEquals(
            listOf(
                "ime set $gboard",
                "settings delete global ${ImeRestore.SETTING_KEY}",
            ),
            ImeRestore.restoreCommands(originalIme = gboard, wasEnabled = true, tapwingImeId = tapwing),
        )
        assertEquals(
            listOf(
                "ime disable $tapwing",
                "settings delete global ${ImeRestore.SETTING_KEY}",
            ),
            ImeRestore.restoreCommands(originalIme = null, wasEnabled = false, tapwingImeId = tapwing),
        )
        assertEquals(
            listOf("settings delete global ${ImeRestore.SETTING_KEY}"),
            ImeRestore.restoreCommands(originalIme = null, wasEnabled = true, tapwingImeId = tapwing),
        )
    }

    @Test
    fun explainsWhenDirectSetTextMustBeSkipped() {
        assertEquals(
            "focused node android.widget.TextView is not editable",
            ImeRestore.setTextSkipReason(
                editable = false,
                password = false,
                supportsSetText = true,
                className = "android.widget.TextView",
            ),
        )
        assertEquals(
            "password field",
            ImeRestore.setTextSkipReason(
                editable = true,
                password = true,
                supportsSetText = true,
                className = "android.widget.EditText",
            ),
        )
        assertEquals(
            "no ACTION_SET_TEXT on io.flutter.embedding.android.FlutterView",
            ImeRestore.setTextSkipReason(
                editable = true,
                password = false,
                supportsSetText = false,
                className = "io.flutter.embedding.android.FlutterView",
            ),
        )
        assertNull(
            ImeRestore.setTextSkipReason(
                editable = true,
                password = false,
                supportsSetText = true,
                className = "android.widget.EditText",
            ),
        )
    }
}

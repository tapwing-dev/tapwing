package dev.tapwing.driver.input

/**
 * The record the driver leaves in `settings` (global) before it selects its
 * own input method, for the host to act on when the driver is killed before it
 * can switch back (`force_stop_command` in the CLI's `native_lane/command.rs`,
 * which reads the same key and format).
 */
internal object ImeRestore {

    const val SETTING_KEY = "tapwing_ime_restore"

    /** No default input method was set when the driver took over. */
    private const val NO_DEFAULT = "-"

    /**
     * `<previous default IME or ->,<1 when the Tapwing IME was already enabled, else 0>`.
     * Free of whitespace: `Shell.run` splits its command on spaces.
     */
    fun record(originalIme: String?, wasEnabled: Boolean): String {
        val ime = originalIme?.takeUnless { it.isBlank() || it.any(Char::isWhitespace) || ',' in it } ?: NO_DEFAULT
        return "$ime,${if (wasEnabled) 1 else 0}"
    }

    /**
     * Whether [imeId] is in `enabled_input_methods`: entries are separated by
     * `:` and may carry `;`-separated subtype hash codes after the id.
     */
    fun isEnabled(enabledInputMethods: String, imeId: String): Boolean =
        enabledInputMethods.trim().split(':').any { it.substringBefore(';') == imeId }
}

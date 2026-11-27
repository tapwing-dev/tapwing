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

    /**
     * Normalizes `settings get secure default_input_method` output into the
     * previous third-party IME id, or `null` when unset / already Tapwing's IME.
     */
    fun normalizePreviousIme(raw: String, tapwingImeId: String): String? =
        raw.trim().takeUnless { it.isBlank() || it == "null" || it == tapwingImeId }

    /**
     * Ordered shell commands executed in `finally` to restore the original IME,
     * disable Tapwing's IME if we enabled it, and delete the crash-recovery marker.
     */
    fun restoreCommands(originalIme: String?, wasEnabled: Boolean, tapwingImeId: String): List<String> = buildList {
        if (originalIme != null) add("ime set $originalIme")
        if (!wasEnabled) add("ime disable $tapwingImeId")
        add("settings delete global $SETTING_KEY")
    }

    /**
     * Returns a diagnostic reason why `ACTION_SET_TEXT` cannot be used directly on
     * a focused node (falling back to the IME), or `null` when direct set-text is allowed.
     */
    fun setTextSkipReason(
        editable: Boolean,
        password: Boolean,
        supportsSetText: Boolean,
        className: CharSequence?,
    ): String? = when {
        !editable -> "focused node $className is not editable"
        password -> "password field"
        !supportsSetText -> "no ACTION_SET_TEXT on $className"
        else -> null
    }
}

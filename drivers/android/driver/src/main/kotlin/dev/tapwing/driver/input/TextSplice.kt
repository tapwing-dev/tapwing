package dev.tapwing.driver.input

/**
 * The text an IME `commitText(text, 1)` would leave in an editor: [inserted]
 * replaces the selection (or goes in at the cursor) and the cursor ends up
 * right after it. Used to reproduce Maestro's Unicode IME result with a single
 * `ACTION_SET_TEXT`, which replaces the whole content.
 */
data class TextSplice(val text: String, val cursor: Int) {

    companion object {
        /**
         * [selectionStart] / [selectionEnd] are `AccessibilityNodeInfo`'s
         * `textSelectionStart` / `textSelectionEnd`: `-1` when unknown, either
         * order (a backwards selection), possibly stale. Unknown or out of
         * range means "append at the end", where a tapped field puts the cursor.
         */
        fun insert(current: String, selectionStart: Int, selectionEnd: Int, inserted: String): TextSplice {
            val known = selectionStart in 0..current.length && selectionEnd in 0..current.length
            val start = if (known) minOf(selectionStart, selectionEnd) else current.length
            val end = if (known) maxOf(selectionStart, selectionEnd) else current.length
            val text = current.substring(0, start) + inserted + current.substring(end)
            return TextSplice(text, start + inserted.length)
        }
    }
}

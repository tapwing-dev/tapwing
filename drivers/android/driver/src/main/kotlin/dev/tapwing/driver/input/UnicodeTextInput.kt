package dev.tapwing.driver.input

import android.app.UiAutomation
import android.os.Build
import android.os.Bundle
import android.provider.Settings
import android.util.Log
import android.view.accessibility.AccessibilityNodeInfo
import android.view.accessibility.AccessibilityNodeInfo.AccessibilityAction
import dev.tapwing.driver.Shell

/**
 * Non-ASCII `inputText` without switching the device's keyboard.
 *
 * Maestro 2.12.1 switches the default IME to its own (`ime set`), commits the
 * text through it (`InputConnection.commitText(text, 1)`: inserted at the
 * cursor, replacing a selection, cursor after it) and switches back. Changing
 * the system IME is slow and flaky (the editor has to rebind to the new IME;
 * an interrupted run leaves the device on the test keyboard), so the primary
 * path reproduces the same result through accessibility instead:
 *
 * 1. find the input-focused editable node;
 * 2. compute what `commitText` would leave ([TextSplice]) from its text and
 *    selection;
 * 3. `ACTION_SET_TEXT` with that text, then `ACTION_SET_SELECTION` to put the
 *    cursor right after the inserted text when that is not the end.
 *
 * Fallback, Maestro's way with an IME in this APK (restored before returning),
 * when there is no focused editable node, the node is a password field (its
 * text is masked, so the splice cannot be computed), or it does not offer or
 * refuses `ACTION_SET_TEXT`.
 */
internal class UnicodeTextInput(
    private val uiAutomation: UiAutomation,
    private val shell: Shell,
) {

    /** Which path the last [input] took (for logs and tests). */
    enum class Path { SET_TEXT, IME }

    fun input(text: String): Path {
        val node = focusedEditable()
        val skipped = if (node == null) {
            "no input-focused node"
        } else {
            try {
                setTextAtCursor(node, text) ?: return Path.SET_TEXT
            } finally {
                @Suppress("DEPRECATION")
                node.recycle()
            }
        }
        Log.i(TAG, "ACTION_SET_TEXT not used ($skipped)")
        inputThroughIme(text)
        return Path.IME
    }

    @Suppress("DEPRECATION")
    private fun recycleQuietly(node: AccessibilityNodeInfo?) {
        if (node != null) runCatching { node.recycle() }
    }

    /**
     * The editable node with input focus: the active window first, then every
     * other window. `findFocus(FOCUS_INPUT)` can return a host view whose
     * focused editor is a virtual descendant (Chrome's web contents before its
     * tree is complete), so a non-editable result is followed by a bounded
     * search of that window for a focused editable node.
     */
    private fun focusedEditable(): AccessibilityNodeInfo? {
        val windows = runCatching { uiAutomation.windows }.getOrDefault(emptyList())
        val orderedWindows = windows.sortedByDescending { it.isFocused }
        val rootProviders: List<() -> AccessibilityNodeInfo?> =
            listOf({ uiAutomation.rootInActiveWindow }) + orderedWindows.map { window -> { window.root } }

        for (provideRoot in rootProviders) {
            val root = provideRoot() ?: continue
            var returned: AccessibilityNodeInfo? = null
            try {
                val focus = root.findFocus(AccessibilityNodeInfo.FOCUS_INPUT) ?: continue
                if (focus.isEditable) {
                    returned = focus
                    return focus
                }
                if (focus !== root) recycleQuietly(focus)
                val descendant = focusedEditableWithin(root)
                if (descendant != null) {
                    returned = descendant
                    return descendant
                }
            } finally {
                if (returned !== root) recycleQuietly(root)
            }
        }
        return null
    }

    private fun focusedEditableWithin(root: AccessibilityNodeInfo): AccessibilityNodeInfo? {
        val queue = ArrayDeque<AccessibilityNodeInfo>().apply { add(root) }
        var visited = 0
        var found: AccessibilityNodeInfo? = null
        try {
            while (queue.isNotEmpty() && visited < MAX_SEARCHED_NODES) {
                val node = queue.removeFirst()
                visited++
                if (node.isFocused && node.isEditable) {
                    found = node
                    return node
                }
                for (i in 0 until node.childCount) {
                    node.getChild(i)?.let(queue::add)
                }
                if (node !== root) recycleQuietly(node)
            }
            return null
        } finally {
            while (queue.isNotEmpty()) {
                val leftover = queue.removeFirst()
                if (leftover !== root && leftover !== found) {
                    recycleQuietly(leftover)
                }
            }
        }
    }

    /** `null` when the text was set, else why not. */
    private fun setTextAtCursor(node: AccessibilityNodeInfo, inserted: String): String? {
        if (!node.isEditable) return "focused node ${node.className} is not editable"
        if (node.isPassword) return "password field"
        if (AccessibilityAction.ACTION_SET_TEXT !in node.actionList) return "no ACTION_SET_TEXT on ${node.className}"
        val showingHint = Build.VERSION.SDK_INT >= Build.VERSION_CODES.O && node.isShowingHintText
        val current = if (showingHint) "" else node.text?.toString().orEmpty()
        val splice = TextSplice.insert(current, node.textSelectionStart, node.textSelectionEnd, inserted)

        val args = Bundle().apply {
            putCharSequence(AccessibilityNodeInfo.ACTION_ARGUMENT_SET_TEXT_CHARSEQUENCE, splice.text)
        }
        if (!node.performAction(AccessibilityNodeInfo.ACTION_SET_TEXT, args)) return "ACTION_SET_TEXT refused"
        if (splice.cursor != splice.text.length) {
            val selection = Bundle().apply {
                putInt(AccessibilityNodeInfo.ACTION_ARGUMENT_SELECTION_START_INT, splice.cursor)
                putInt(AccessibilityNodeInfo.ACTION_ARGUMENT_SELECTION_END_INT, splice.cursor)
            }
            node.performAction(AccessibilityNodeInfo.ACTION_SET_SELECTION, selection)
        }
        return null
    }

    /**
     * Maestro's `inputUnicodeText`, with the IME living in this process.
     * Serialized across gRPC threads so concurrent calls cannot race on
     * `ime enable` / `ime set` / `ime disable`.
     */
    private fun inputThroughIme(text: String) = synchronized(imeLock) {
        // Read as the shell user: apps targeting API 34+ may not read
        // `enabled_input_methods` through Settings.Secure.
        val originalIme = shell.run("settings get secure ${Settings.Secure.DEFAULT_INPUT_METHOD}").trim()
            .takeUnless { it.isBlank() || it == "null" || it == TapwingInputMethodService.IME_ID }
        val wasEnabled = ImeRestore.isEnabled(
            shell.run("settings get secure ${Settings.Secure.ENABLED_INPUT_METHODS}"),
            TapwingInputMethodService.IME_ID,
        )
        Log.i(TAG, "Unicode input through the Tapwing IME (was $originalIme)")
        // The host stops this process with `am force-stop`, which skips the
        // `finally` below. Leave it a record of what to put back, so the
        // device does not stay on a keyboard without keys.
        runCatching {
            shell.run("settings put global ${ImeRestore.SETTING_KEY} ${ImeRestore.record(originalIme, wasEnabled)}")
        }
        try {
            shell.run("ime enable ${TapwingInputMethodService.IME_ID}")
            shell.run("ime set ${TapwingInputMethodService.IME_ID}")
            awaitIme()
            TextChunks.split(text).forEach(TapwingInputMethodService::commit)
            Thread.sleep(COMMIT_SETTLE_DELAY_MS)
        } finally {
            originalIme?.let { runCatching { shell.run("ime set $it") } }
            if (!wasEnabled) runCatching { shell.run("ime disable ${TapwingInputMethodService.IME_ID}") }
            runCatching { shell.run("settings delete global ${ImeRestore.SETTING_KEY}") }
        }
    }

    private fun awaitIme() {
        val deadline = System.currentTimeMillis() + IME_READY_TIMEOUT_MS
        while (true) {
            val reason = TapwingInputMethodService.notReadyReason() ?: return
            if (System.currentTimeMillis() >= deadline) throw IllegalStateException(reason)
            Thread.sleep(IME_POLL_INTERVAL_MS)
        }
    }

    private companion object {
        const val TAG = "TapwingDriver"
        // `AndroidDriver.MAESTRO_IME_READY_TIMEOUT_MS` / `MAESTRO_IME_COMMIT_SETTLE_DELAY_MS`.
        const val IME_READY_TIMEOUT_MS = 5_000L
        const val COMMIT_SETTLE_DELAY_MS = 250L
        const val IME_POLL_INTERVAL_MS = 50L
        const val MAX_SEARCHED_NODES = 5_000
        val imeLock = Any()
    }
}

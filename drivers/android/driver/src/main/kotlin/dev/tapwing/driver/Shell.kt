package dev.tapwing.driver

import android.app.UiAutomation
import android.os.ParcelFileDescriptor

/** Runs commands as the `shell` user through the instrumentation's UiAutomation. */
class Shell(private val uiAutomation: UiAutomation) {

    /** Runs [command] (no shell syntax: it is split on spaces) and returns its stdout. */
    fun run(command: String): String {
        val pfd = uiAutomation.executeShellCommand(command)
            ?: throw IllegalStateException("UiAutomation.executeShellCommand returned null for: $command")
        return ParcelFileDescriptor.AutoCloseInputStream(pfd).use { it.readBytes().toString(Charsets.UTF_8) }
    }
}

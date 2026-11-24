// Copyright (c) 2022 mobile.dev inc.
// Modifications copyright (c) 2026 the Tapwing authors.
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.
//
// Ported from Maestro 2.12.1 (github.com/mobile-dev-inc/maestro, tag cli-2.12.1),
// maestro-android/src/main/java/dev/mobile/maestro/input/MaestroInputMethodService.kt.
// Changes: no broadcast receiver. The system binds this IME into the driver's
// own process (the instrumentation targets its own package), so the gRPC
// thread hands text to it directly and the commit runs on the main looper.
package dev.tapwing.driver.input

import android.inputmethodservice.InputMethodService
import android.os.Handler
import android.os.Looper
import android.util.Log
import android.view.View
import android.view.inputmethod.EditorInfo
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicReference

/**
 * Fallback Unicode input: an invisible IME whose only job is `commitText`.
 * Only active while [UnicodeTextInput] switches to it for one `inputText`.
 */
class TapwingInputMethodService : InputMethodService() {

    override fun onCreate() {
        super.onCreate()
        activeService.set(this)
        hasActiveInputConnection.set(false)
        Log.i(TAG, "Tapwing IME created")
    }

    override fun onCreateInputView(): View = View(this)

    override fun onStartInput(attribute: EditorInfo?, restarting: Boolean) {
        super.onStartInput(attribute, restarting)
        hasActiveInputConnection.set(currentInputConnection != null)
    }

    override fun onStartInputView(info: EditorInfo?, restarting: Boolean) {
        super.onStartInputView(info, restarting)
        hasActiveInputConnection.set(currentInputConnection != null)
    }

    override fun onFinishInput() {
        hasActiveInputConnection.set(false)
        super.onFinishInput()
    }

    override fun onDestroy() {
        activeService.compareAndSet(this, null)
        hasActiveInputConnection.set(false)
        Log.i(TAG, "Tapwing IME destroyed")
        super.onDestroy()
    }

    override fun onEvaluateFullscreenMode(): Boolean = false

    private fun commitOnMainThread(text: String): String? {
        val inputConnection = currentInputConnection ?: return "No active input connection"
        inputConnection.beginBatchEdit()
        val committed = try {
            val ok = inputConnection.commitText(text, 1)
            inputConnection.finishComposingText()
            ok
        } finally {
            inputConnection.endBatchEdit()
        }
        return if (committed) null else "commitText returned false"
    }

    companion object {
        const val IME_ID = "dev.tapwing.driver/.input.TapwingInputMethodService"
        private const val TAG = "TapwingIME"
        private const val COMMIT_TIMEOUT_MS = 5_000L

        private val activeService = AtomicReference<TapwingInputMethodService?>()
        private val hasActiveInputConnection = AtomicReference(false)
        private val mainHandler = Handler(Looper.getMainLooper())

        /** `null` when bound to an editor and ready to commit, else why not. */
        fun notReadyReason(): String? {
            val service = activeService.get() ?: return "Tapwing IME is not active"
            if (!hasActiveInputConnection.get() || service.currentInputConnection == null) {
                return "No active input connection"
            }
            return null
        }

        /** Commits [text] at the cursor (replacing any selection), like a keyboard would. */
        fun commit(text: String) {
            notReadyReason()?.let { throw IllegalStateException(it) }
            val service = activeService.get() ?: throw IllegalStateException("Tapwing IME is not active")
            val error = AtomicReference<String?>("Timed out committing text")
            val done = CountDownLatch(1)
            mainHandler.post {
                error.set(runCatching { service.commitOnMainThread(text) }.getOrElse { it.message ?: it.javaClass.simpleName })
                done.countDown()
            }
            if (!done.await(COMMIT_TIMEOUT_MS, TimeUnit.MILLISECONDS)) {
                throw IllegalStateException("Timed out committing text")
            }
            error.get()?.let { throw IllegalStateException(it) }
        }
    }
}

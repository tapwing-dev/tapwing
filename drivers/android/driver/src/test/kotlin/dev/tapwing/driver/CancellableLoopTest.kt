package dev.tapwing.driver

import dev.tapwing.driver.input.CancellableLoop
import dev.tapwing.driver.input.TypingCancelledException
import org.junit.Assert.assertEquals
import org.junit.Test

class CancellableLoopTest {

    /** Runs the loop over [text], cancelling once [cancelAfter] keys were sent. */
    private fun type(text: String, cancelAfter: Int?): Pair<Int, String> {
        val sent = StringBuilder()
        val done = CancellableLoop.run(text.length, { cancelAfter != null && sent.length >= cancelAfter }) { i ->
            sent.append(text[i])
        }
        return done to sent.toString()
    }

    @Test
    fun aCallThatStaysOpenGetsEveryKey() {
        assertEquals(5 to "hello", type("hello", cancelAfter = null))
        assertEquals(0 to "", type("", cancelAfter = null))
    }

    @Test
    fun noKeyIsSentAfterTheCallWasCancelled() {
        assertEquals(3 to "hel", type("hello world", cancelAfter = 3))
        assertEquals(1 to "h", type("hello", cancelAfter = 1))
        // Cancelled only once the last key is out: nothing was cut short.
        assertEquals(5 to "hello", type("hello", cancelAfter = 5))
    }

    @Test
    fun aCallCancelledBeforeTheFirstKeySendsNothing() {
        assertEquals(0 to "", type("hello", cancelAfter = 0))
    }

    @Test
    fun cancellationIsCheckedBeforeEveryStepAndNeverAfterTheLast() {
        val events = mutableListOf<String>()
        val done = CancellableLoop.run(3, { events.add("check"); false }) { events.add("key$it") }
        assertEquals(3, done)
        assertEquals(listOf("check", "key0", "check", "key1", "check", "key2"), events)
    }

    @Test
    fun aCancelledLoopStopsAskingAndStepping() {
        var checks = 0
        val steps = mutableListOf<Int>()
        val done = CancellableLoop.run(1_000, { ++checks > 2 }) { steps.add(it) }
        assertEquals(2, done)
        assertEquals(listOf(0, 1), steps)
        assertEquals(3, checks)
    }

    @Test
    fun theErrorSaysHowFarTypingGot() {
        val e = TypingCancelledException(done = 400, total = 1000, unit = "keys")
        assertEquals("cancelled after 400 of 1000 keys: the client is gone or its deadline passed", e.message)
        assertEquals(400 to 1000, e.done to e.total)
    }
}

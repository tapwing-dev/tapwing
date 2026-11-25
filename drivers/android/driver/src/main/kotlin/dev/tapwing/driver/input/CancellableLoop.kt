package dev.tapwing.driver.input

/**
 * The loop under `inputText` and `eraseAllText`: one step per key or text
 * chunk, with a look at the call before each.
 *
 * A long text takes longer than the client's RPC deadline (75 ms per ASCII
 * key). When the deadline passes, or the client goes away, gRPC cancels the
 * call but the handler thread keeps running, so without this check the
 * driver would go on typing into whatever the next step put on screen.
 *
 * Pure (no Android, no gRPC), so it is unit-tested on the JVM.
 */
internal object CancellableLoop {

    /**
     * Runs [step] for 0 until [times], asking [isCancelled] before each one
     * and stopping at the first `true`.
     *
     * @return the number of steps run: [times] when the loop finished, fewer
     * when it was cancelled.
     */
    inline fun run(times: Int, isCancelled: () -> Boolean, step: (Int) -> Unit): Int {
        for (index in 0 until times) {
            if (isCancelled()) return index
            step(index)
        }
        return times
    }
}

/** Typing stopped because the call was cancelled; [done] of [total] [unit] had been sent. */
internal class TypingCancelledException(val done: Int, val total: Int, unit: String) :
    RuntimeException("cancelled after $done of $total $unit: the client is gone or its deadline passed")

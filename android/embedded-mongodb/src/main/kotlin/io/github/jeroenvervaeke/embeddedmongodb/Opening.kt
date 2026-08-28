package io.github.jeroenvervaeke.embeddedmongodb

import kotlin.coroutines.cancellation.CancellationException
import kotlin.coroutines.coroutineContext
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.NonCancellable
import kotlinx.coroutines.ensureActive
import kotlinx.coroutines.withContext

/**
 * Runs [open] off the caller's thread and guarantees the database it produces either reaches the
 * caller or is closed.
 *
 * `withContext(Dispatchers.IO) { … }` on its own does not, and the gap matters here more than it
 * usually would. Opening has no suspension point, so cancelling the caller cannot stop it: the
 * engine comes up either way, and `withContext` then throws `CancellationException` and drops the
 * database it was handed. Only one runtime may exist per process, so that engine is one nobody
 * holds a handle to and nothing can close — every later open in the process fails with "a second
 * runtime", and only killing the application clears it. On Android the cancellation that causes
 * it is ordinary: a screen left, or a rotation, while the engine is still starting.
 *
 * Cancellation costs the caller nothing extra: the open was never interruptible, so waiting for
 * it to finish is what was happening anyway. What changes is that the engine it produced is
 * closed rather than abandoned.
 */
internal suspend fun openedOrClosed(open: () -> EmbeddedMongo): EmbeddedMongo {
    val caller = coroutineContext
    return withContext(NonCancellable + Dispatchers.IO) {
        val database = open()
        try {
            caller.ensureActive()
        } catch (cancelled: CancellationException) {
            // The last chance to close it: nothing else holds the handle.
            try {
                database.close()
            } catch (closing: Throwable) {
                cancelled.addSuppressed(closing)
            }
            throw cancelled
        }
        database
    }
}

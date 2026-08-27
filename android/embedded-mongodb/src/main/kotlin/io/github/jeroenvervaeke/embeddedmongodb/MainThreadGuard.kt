package io.github.jeroenvervaeke.embeddedmongodb

import android.os.Looper
import android.util.Log

/**
 * Keeps engine calls off Android's main thread.
 *
 * A query over a few thousand documents outlasts the ANR budget comfortably, and neither the
 * engine nor JNI offers a way to interrupt one, so a blocking call from the main thread fails
 * immediately instead of freezing the UI until the watchdog kills the process.
 *
 * [warn] is the softer half, used by [EmbeddedMongo.close]: throwing there would replace the
 * exception that sent the caller into `use { }`, and closing from a lifecycle callback — which
 * runs on the main thread — is a reasonable thing to do.
 */
internal class MainThreadGuard(
    private val onMainThread: () -> Boolean,
    private val report: (String) -> Unit = { Log.w(TAG, it) },
) {
    fun reject(operation: String) {
        if (!onMainThread()) return
        error(complaint(operation))
    }

    fun warn(operation: String) {
        if (!onMainThread()) return
        report(complaint(operation))
    }

    private fun complaint(operation: String) =
        "$operation on the main thread blocks the UI for as long as the engine takes, which is " +
            "long enough to trigger an ANR. Call it from a background thread, or use the " +
            "suspending API, which dispatches onto the database thread."

    companion object {
        private const val TAG = "EmbeddedMongo"

        /**
         * A thread that never prepared a Looper — every thread this library expects to run on —
         * reports `null`, which is why the comparison is against the main Looper rather than
         * against `null`.
         */
        val Android = MainThreadGuard(onMainThread = { Looper.myLooper() == Looper.getMainLooper() })
    }
}

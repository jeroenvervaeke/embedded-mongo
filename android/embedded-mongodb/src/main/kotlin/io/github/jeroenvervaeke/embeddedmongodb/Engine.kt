package io.github.jeroenvervaeke.embeddedmongodb

import java.io.File
import java.util.concurrent.atomic.AtomicLong

/**
 * The seam between [EmbeddedMongo] and the native library: above it everything is a
 * [org.bson.Document], below it everything is BSON bytes.
 *
 * Cursor paging, error mapping and thread policy sit above the seam, so unit tests reach all of
 * them through a fake engine — without a device, an emulator or a compiled Rust crate.
 */
internal interface Engine : AutoCloseable {
    fun command(database: String, command: ByteArray): ByteArray

    override fun close()
}

/** The [Engine] that talks to the Rust crate through [NativeBridge]. */
internal class NativeEngine private constructor(handle: Long) : Engine {
    // No lock around the calls: the registry behind the bridge takes a shared lock for a command
    // and an exclusive one for close, so close waits for the commands already running and a handle
    // that is gone is a clean lookup miss rather than a dangling pointer. What is left for this
    // side is publishing the handle between threads, and making sure only one caller can hand a
    // handle to close -- the bridge reports a second release as an error.
    private val handle = AtomicLong(handle)

    override fun command(database: String, command: ByteArray): ByteArray =
        NativeBridge.command(openHandle(), database, command)

    override fun close() {
        val closing = handle.getAndSet(CLOSED)
        if (closing != CLOSED) NativeBridge.close(closing)
    }

    private fun openHandle(): Long {
        val open = handle.get()
        check(open != CLOSED) { "the embedded MongoDB database is closed" }
        return open
    }

    companion object {
        private const val CLOSED = 0L

        // The engine resolves the path against a working directory that is not the
        // application's, so a relative one would open a database somewhere unexpected.
        fun open(directory: File): NativeEngine = NativeEngine(NativeBridge.open(directory.absolutePath))
    }
}

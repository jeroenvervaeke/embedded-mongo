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

/**
 * The two entry points the native library exports for opening.
 *
 * Behind an interface for the same reason [Engine] is one: which of them a caller reaches is a
 * compatibility promise — an application that names no limit must keep reaching the one that
 * predates them — and a promise a test cannot see is one nothing holds to.
 */
internal interface BridgeOpener {
    fun open(path: String): Long

    fun openWithOptions(path: String, options: LongArray): Long
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

        /**
         * Opens through whichever entry point [options] calls for, which is the one that
         * predates them whenever a caller names no limit at all. An application that asks for
         * nothing therefore reaches nothing new — no second code path, and no symbol an older
         * native library would not have exported.
         */
        fun open(
            directory: File,
            options: StorageOptions = StorageOptions(),
            opener: BridgeOpener = NativeBridgeOpener,
        ): NativeEngine {
            // The engine resolves the path against a working directory that is not the
            // application's, so a relative one would open a database somewhere unexpected.
            val path = directory.absolutePath
            val slots = options.engineSlots()
            val handle =
                if (slots.isEmpty()) opener.open(path)
                else opener.openWithOptions(path, slots)
            return NativeEngine(handle)
        }
    }
}

/** The [BridgeOpener] every caller but a test gets. */
private object NativeBridgeOpener : BridgeOpener {
    override fun open(path: String): Long = NativeBridge.open(path)

    override fun openWithOptions(path: String, options: LongArray): Long =
        NativeBridge.openWithOptions(path, options)
}

/**
 * The storage limits as `NativeBridge.openWithOptions` reads them: one slot each, in the units
 * the engine takes, and zero wherever the caller named nothing.
 *
 * Trimmed to the last slot that was named, so a caller who sets only the cache sends one slot
 * rather than three. That is the same array a library older than its caller would receive, so
 * the growth rule the bridge documents is exercised by ordinary use rather than only by a
 * test. Trimming can only ever drop zeros, which are what "not named" is spelled as.
 *
 * [StorageOptions.freeDiskFloor] is deliberately absent, and adding a slot for it would
 * reintroduce a defect rather than merely blur a layer. Both layers record "MongoDB's own
 * floors" at their first open, and they agree only because the floor never crosses this
 * vector: the crate has already put the engine's defaults back before Kotlin takes its
 * reading. Carry the floor across here and Kotlin records the first caller's floor as the
 * default and holds it for the life of the process. `embedded-mongodb-android/src/options.rs`
 * has the long version.
 */
internal fun StorageOptions.engineSlots(): LongArray {
    val slots = longArrayOf(
        cacheSize?.mebibytes?.toLong() ?: UNSET,
        journalFileSize?.kibibytes?.toLong() ?: UNSET,
        journalPreallocation?.slot ?: UNSET,
    )
    return slots.copyOf(slots.indexOfLast { it != UNSET } + 1)
}

/** What the bridge reads as "leave this one to the engine". */
private const val UNSET = 0L

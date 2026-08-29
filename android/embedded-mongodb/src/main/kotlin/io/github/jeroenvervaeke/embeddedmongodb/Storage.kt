package io.github.jeroenvervaeke.embeddedmongodb

import android.content.Context
import android.os.storage.StorageManager
import java.io.File
import java.io.IOException

/**
 * Thrown when the volume a database would live on cannot give the engine room to work.
 *
 * Worth catching rather than letting through: [requiredBytes] minus [allocatableBytes] is what an
 * application would have to free, or ask the user to free, before opening can work.
 */
class InsufficientStorageException internal constructor(
    val allocatableBytes: Long,
    val requiredBytes: Long,
) : Exception(
    "the embedded MongoDB engine needs ${requiredBytes / BYTES_PER_MEBIBYTE} MiB to open a " +
        "database, and this volume can give it ${allocatableBytes / BYTES_PER_MEBIBYTE} MiB",
)

/**
 * Refuses to open a database on a volume without the room the engine needs, or without the
 * room [floor] asks for where a caller named one.
 *
 * This is the one precondition worth checking before calling into the engine, because running out
 * of space is not an error it returns: WiredTiger panics and the process is aborted, past the
 * reach of any `catch`. What this buys is a typed exception, thrown before the engine is asked at
 * all, naming how much room there is and how much is wanted.
 *
 * The measurement is [StorageManager.getAllocatableBytes], not `File.usableSpace`: it counts the
 * cached data Android is willing to delete on the application's behalf, so it does not refuse to
 * open on a device that is merely holding reclaimable cache. An application that wants that space
 * reclaimed rather than counted can call [StorageManager.allocateBytes] first.
 *
 * Advisory by design: a platform that will not answer leaves the decision to the engine rather
 * than blocking an open that might have worked.
 */
internal fun checkStorage(context: Context, directory: File, options: StorageOptions) {
    val allocatable = allocatableBytes(context, directory) ?: return
    checkAllocatable(allocatable, options)
}

internal fun checkAllocatable(allocatableBytes: Long, options: StorageOptions) {
    val required = requiredFreeBytes(options)
    if (allocatableBytes >= required) return
    throw InsufficientStorageException(allocatableBytes, required)
}

/**
 * Room for the engine to work in, lowered to whatever floor the caller named but never below
 * what the engine needs to open at all.
 *
 * An application that sets [StorageOptions.freeDiskFloor] to 64 MiB has said that 64 MiB of
 * headroom is enough for the work it is about to do, and refusing to open it on 200 MB would
 * take back the one knob that makes a nearly-full device usable. The floor governs index builds
 * and spilling queries, though, not whether WiredTiger can create its first journal file — so a
 * floor lower than [bytesToOpen] must not drag this check down with it, or the check would wave
 * through exactly the volume it exists to catch.
 *
 * It only ever lowers past the default: raising the engine's floor says nothing about how much
 * the *platform* will hand this application, which is the smaller number this check is against
 * and the reason it has a default of its own.
 */
internal fun requiredFreeBytes(options: StorageOptions): Long = maxOf(
    bytesToOpen(options),
    minOf(DEFAULT_REQUIRED_FREE_BYTES, options.freeDiskFloor?.bytes ?: DEFAULT_REQUIRED_FREE_BYTES),
)

/**
 * What the engine must be able to write before it can answer anything.
 *
 * WiredTiger allocates a journal file in full the moment it creates it, and keeps a second one
 * ready when a spare is asked for, so the journal is nearly the whole of it. Everything else a
 * fresh directory holds — the catalog, the history store, the turtle file, the size storer and
 * the scratch database for spilling — measured about 130 KiB together, which [OPEN_MARGIN_BYTES]
 * covers several times over.
 *
 * The 8 MiB is the engine's own default restated, which is normally something this side refuses
 * to do. It is unavoidable here: predicting the cost of an open before making it is the one job
 * that cannot ask the engine what its default is.
 */
private fun bytesToOpen(options: StorageOptions): Long {
    val kibibytes = options.journalFileSize?.kibibytes ?: DEFAULT_JOURNAL_KIBIBYTES
    val files = if (options.journalPreallocation == JournalPreallocation.ENABLED) 2 else 1
    return kibibytes.toLong() * 1024 * files + OPEN_MARGIN_BYTES
}

/**
 * What the volume holding [directory] could give this application, or `null` when the platform
 * will not say.
 *
 * `getAllocatableBytes` arrived in API 26, which is this library's floor, so it is always there
 * to call. `usableSpace` is deliberately not a fallback: it ignores reclaimable cache and would
 * refuse to open where the engine would have succeeded.
 */
internal fun allocatableBytes(context: Context, directory: File): Long? {
    val storage = context.getSystemService(StorageManager::class.java) ?: return null
    return try {
        storage.getAllocatableBytes(storage.getUuidForPath(directory))
    } catch (error: IOException) {
        // Thrown for a volume that is not one the platform tracks -- an app-private directory on
        // removable storage, say. Not a reason to refuse: the engine still checks for itself.
        null
    }
}

/**
 * Room for the engine to work in when the caller named no floor of their own, and deliberately
 * below the 500 MB of free space the engine itself insists on before it will build an index.
 *
 * The two numbers measure different things and cannot be the same. The engine asks the filesystem
 * how much space is free; `getAllocatableBytes` answers how much *this application* could be given,
 * which is the smaller number — comfortably less than half of it on a device holding data for other
 * applications. Setting this to the engine's own figure would refuse to open on devices where the
 * engine opens without complaint, which is the failure this check must not have: its job is to
 * catch the volume that has nothing left, before the engine hits it and aborts the process.
 */
private const val DEFAULT_REQUIRED_FREE_BYTES = 256L * BYTES_PER_MEBIBYTE

/** The engine's own default journal file size; see [bytesToOpen] for why it is repeated here. */
private const val DEFAULT_JOURNAL_KIBIBYTES = 8 * 1024

/** Room for everything a fresh directory holds besides its journal, with room to spare. */
private const val OPEN_MARGIN_BYTES = BYTES_PER_MEBIBYTE

package io.github.jeroenvervaeke.embeddedmongodb

import android.content.Context
import android.os.Build
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
    "the embedded MongoDB engine needs ${requiredBytes / BYTES_PER_MEBIBYTE} MB to open a " +
        "database, and this volume can give it ${allocatableBytes / BYTES_PER_MEBIBYTE} MB",
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
internal fun checkStorage(context: Context, directory: File, floor: FreeDiskFloor?) {
    val allocatable = allocatableBytes(context, directory) ?: return
    checkAllocatable(allocatable, floor)
}

internal fun checkAllocatable(allocatableBytes: Long, floor: FreeDiskFloor?) {
    val required = requiredFreeBytes(floor)
    if (allocatableBytes >= required) return
    throw InsufficientStorageException(allocatableBytes, required)
}

/**
 * Room for the engine to work in, lowered to whatever floor the caller named.
 *
 * An application that sets [StorageOptions.freeDiskFloor] to 64 MiB has said that 64 MiB of
 * headroom is enough for the work it is about to do, and refusing to open it on 200 MB would
 * take back the one knob that makes a nearly-full device usable. It only ever lowers: raising
 * the engine's floor says nothing about how much the *platform* will hand this application,
 * which is the smaller number this check is against and the reason it has a default of its own.
 */
internal fun requiredFreeBytes(floor: FreeDiskFloor?): Long =
    minOf(DEFAULT_REQUIRED_FREE_BYTES, floor?.bytes ?: DEFAULT_REQUIRED_FREE_BYTES)

/**
 * What the volume holding [directory] could give this application, or `null` when the platform
 * will not say.
 *
 * `getAllocatableBytes` arrived in API 26, two levels above this library's floor. The gap is
 * deliberately left unmeasured rather than filled with `usableSpace`, which ignores reclaimable
 * cache and would refuse to open where the engine would have succeeded.
 */
internal fun allocatableBytes(context: Context, directory: File): Long? {
    if (Build.VERSION.SDK_INT < Build.VERSION_CODES.O) return null
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

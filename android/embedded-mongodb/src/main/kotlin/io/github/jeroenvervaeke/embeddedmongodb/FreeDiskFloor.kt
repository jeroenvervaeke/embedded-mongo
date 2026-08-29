package io.github.jeroenvervaeke.embeddedmongodb

import org.bson.Document

/**
 * How much free disk space an index build or a spilling query insists on before it starts.
 *
 * MongoDB refuses to start an index build, and refuses to spill a query to disk, when the data
 * directory has less than 500 MB free. That is sized for a server. A phone near its limit does
 * not have 500 MB free at all, so on such a device an application that can open and read its
 * database still cannot build an index over it: seeding on first launch fails at
 * `createIndexes` with `OutOfDiskSpace`, and a query that has to spill fails the same way.
 * Lowering the floor is what makes that device work.
 *
 * ## What lowering it costs
 *
 * The floor is a pre-flight check and nothing else. It refuses a build that would start with
 * too little room; **nothing stops one that runs out part-way.** This engine runs no
 * `DiskSpaceMonitor` — the thread mongod uses to abort builds as a disk fills is started from
 * `mongod_main`, which this engine does not use — and WiredTiger answers a genuinely full disk
 * with `WT_PANIC`, which MongoDB answers with `fassert`. That aborts the application process.
 * No exception is thrown, nothing is returned, and there is nothing to catch.
 *
 * So the floor is the only warning an application gets, and lowering it trades a clean refusal
 * it can report to the user for a crash it cannot. How much headroom is enough depends on how
 * much data is about to be indexed, which the application knows and this library does not:
 * lower it to what the work about to be done actually needs, not to what will fit.
 *
 * Worth pairing with [InsufficientStorageException], which is thrown before the engine is
 * opened at all: a floor named here also lowers the room that check insists on, since an
 * application that says 64 MiB is enough should not be refused at 256 MB.
 *
 * ## It is process-global, not per-database
 *
 * Both floors are MongoDB **server parameters**, and this engine keeps one runtime for the whole
 * life of the process. A floor is therefore a setting of the *process*, not of the
 * [EmbeddedMongo] that named it: it survives that instance's [EmbeddedMongo.close], and left
 * alone it would still be in force for the next [EmbeddedMongo.open]. That is not guessable from
 * an API where the floor is named per-open, so this library does not leave it to be discovered —
 * **every open establishes the floor**, putting MongoDB's own back where the caller named none.
 * An application that opens one database on a lowered floor, closes it and opens another gets
 * the defaults it asked for rather than the previous database's floor.
 *
 * Two consequences worth knowing. A floor moved with [setFreeDiskFloor] on a running database
 * lasts until the next [EmbeddedMongo.open], which resets it — it is not remembered for a
 * database, so an application that wants it every time names it in [StorageOptions.freeDiskFloor]
 * rather than setting it after opening. And while a database is open the floor is shared by every
 * database name inside it, because there is only ever one engine to set it on.
 */
@JvmInline
value class FreeDiskFloor private constructor(val mebibytes: Int) {
    /** The same floor in the unit the query-spilling knob takes. */
    internal val bytes: Long get() = mebibytes.toLong() * BYTES_PER_MEBIBYTE

    companion object {
        /**
         * One mebibyte is the lowest floor that can be asked for, and is in practice no floor
         * at all. Zero is excluded because `indexBuildMinAvailableDiskSpaceMB` compares with
         * `<=`: a floor of zero would still refuse a build on a disk with nothing left, while
         * giving up every megabyte of warning before it.
         */
        const val MIN_MEBIBYTES: Int = 1

        const val MAX_MEBIBYTES: Int = Int.MAX_VALUE

        /** What both knobs hold until something moves them. MongoDB's own default. */
        val ENGINE_DEFAULT: FreeDiskFloor = FreeDiskFloor(500)

        /** @throws IllegalArgumentException if [mebibytes] is not a floor that can be set. */
        fun ofMebibytes(mebibytes: Int): FreeDiskFloor =
            FreeDiskFloor(inRange("free disk floor", "MiB", mebibytes, MIN_MEBIBYTES, MAX_MEBIBYTES))
    }
}

/**
 * The two floors as the engine currently reports them, each in the unit its own knob uses.
 *
 * Not a [FreeDiskFloor]: these come back from the engine rather than going into it, they can
 * disagree with each other if something set them separately, and the spilling one is a byte
 * count that need not be a whole mebibyte.
 */
data class ReportedFloors(val indexBuildMebibytes: Long, val querySpillingBytes: Long)

/**
 * Applies [floor] to a database that is already open, and suspends until the engine has it.
 *
 * [StorageOptions.freeDiskFloor] is the usual way to reach this, and applies it during
 * [EmbeddedMongo.open]. This is here as well because the floor is the one limit that can move
 * while running: raise it before a large index build and drop it again afterwards.
 *
 * What moves is a pair of server parameters belonging to the process rather than to this
 * database — see [FreeDiskFloor]. It reaches every database name this engine serves, and the
 * next [EmbeddedMongo.open] puts it back to whatever that open asks for.
 *
 * @throws EmbeddedMongoException if the engine will not take the floor. Returned rather than
 *   logged: an application that asked for a floor and did not get it would otherwise find out
 *   at the index build, on the device where the index build was the thing that had to work.
 *   The floors are put back where they were before the failure, so a refusal leaves the engine
 *   as it was rather than half moved; [freeDiskFloors] reports where they ended up if even that
 *   did not work.
 */
suspend fun EmbeddedMongo.setFreeDiskFloor(floor: FreeDiskFloor) {
    val before = freeDiskFloors()
    try {
        for (knob in freeDiskFloorCommands(floor)) command(ADMIN, knob)
    } catch (failure: Throwable) {
        restore(before, failure) { knob -> command(ADMIN, knob) }
        throw failure
    }
}

/**
 * [setFreeDiskFloor] on the calling thread.
 *
 * @throws IllegalStateException if called on the main thread, or after [EmbeddedMongo.close].
 */
fun EmbeddedMongo.setFreeDiskFloorBlocking(floor: FreeDiskFloor) {
    val before = freeDiskFloorsBlocking()
    try {
        for (knob in freeDiskFloorCommands(floor)) commandBlocking(ADMIN, knob)
    } catch (failure: Throwable) {
        restore(before, failure) { knob -> commandBlocking(ADMIN, knob) }
        throw failure
    }
}

/** What the engine says the two floors are now. */
suspend fun EmbeddedMongo.freeDiskFloors(): ReportedFloors =
    reportedFloors(command(ADMIN, reportedFloorsCommand()))

/**
 * [freeDiskFloors] on the calling thread.
 *
 * @throws IllegalStateException if called on the main thread, or after [EmbeddedMongo.close].
 */
fun EmbeddedMongo.freeDiskFloorsBlocking(): ReportedFloors =
    reportedFloors(commandBlocking(ADMIN, reportedFloorsCommand()))

/**
 * Puts [floors] in force, each knob in the unit it takes.
 *
 * Separate from [setFreeDiskFloor] because a [ReportedFloors] is not a [FreeDiskFloor]: the
 * spilling knob is a byte count that need not be a whole mebibyte, so floors read back from the
 * engine have to be restored to what was read rather than to a number rounded through this
 * library's type.
 */
internal fun EmbeddedMongo.restoreFreeDiskFloorsBlocking(floors: ReportedFloors) {
    for (knob in floors.commands()) commandBlocking(ADMIN, knob)
}

/**
 * Puts the floors back where [before] found them, after a move that did not finish.
 *
 * The two knobs take two commands, so a failure on the second leaves the first already moved --
 * and moved *down*, in the case that matters, so an application that caught the exception and
 * concluded nothing had happened would go on to build an index against a floor far lower than the
 * one it believes is protecting it. That is the trade this module exists to make deliberate, so it
 * is not one to make by accident.
 *
 * A restore that itself fails is attached to the original failure rather than replacing it: the
 * caller is owed the reason their floor was refused first, and [ReportedFloors] is how they find
 * out where the floors actually ended up.
 */
private inline fun restore(before: ReportedFloors, failure: Throwable, send: (Document) -> Unit) {
    try {
        for (knob in before.commands()) send(knob)
    } catch (restoring: Throwable) {
        failure.addSuppressed(restoring)
    }
}

/** The `setParameter` commands that put these floors back, each in the unit its knob takes. */
private fun ReportedFloors.commands(): List<Document> = listOf(
    Document(SET_PARAMETER, 1).append(INDEX_BUILD_FLOOR, indexBuildMebibytes),
    Document(SET_PARAMETER, 1).append(QUERY_SPILLING_FLOOR, querySpillingBytes),
)

/**
 * The two `setParameter` commands that move the floor, in the units each knob takes.
 *
 * Two commands rather than one: `setParameter` reports the previous value in a field named
 * `was`, so a combined command answers with two fields of the same name and a parameter that
 * was quietly rejected is indistinguishable from one that was applied.
 *
 * Both go over [EmbeddedMongo.command], which is all a server parameter needs. That is the
 * whole reason none of this reaches the native bridge: what `setParameter` can do on a running
 * engine does not need a native entry point, an engine rebuild or a release to change.
 */
internal fun freeDiskFloorCommands(floor: FreeDiskFloor): List<Document> = listOf(
    Document(SET_PARAMETER, 1).append(INDEX_BUILD_FLOOR, floor.mebibytes.toLong()),
    Document(SET_PARAMETER, 1).append(QUERY_SPILLING_FLOOR, floor.bytes),
)

internal fun reportedFloorsCommand(): Document =
    Document("getParameter", 1).append(INDEX_BUILD_FLOOR, 1).append(QUERY_SPILLING_FLOOR, 1)

internal fun reportedFloors(reply: Document): ReportedFloors = ReportedFloors(
    indexBuildMebibytes = floorOf(reply, INDEX_BUILD_FLOOR),
    querySpillingBytes = floorOf(reply, QUERY_SPILLING_FLOOR),
)

internal const val BYTES_PER_MEBIBYTE = 1024L * 1024L

private const val ADMIN = "admin"

private const val SET_PARAMETER = "setParameter"

private const val INDEX_BUILD_FLOOR = "indexBuildMinAvailableDiskSpaceMB"

private const val QUERY_SPILLING_FLOOR = "internalQuerySpillingMinAvailableDiskSpaceBytes"

/**
 * A missing knob is raised rather than defaulted: a MongoDB that renamed one of these would
 * otherwise report a floor this library never set, and an application would size its work
 * against a number that is not the one in force.
 */
private fun floorOf(reply: Document, name: String): Long =
    (reply[name] as? Number)?.toLong()
        ?: throw EmbeddedMongoException("getParameter has no $name", NO_ERROR_CODE)

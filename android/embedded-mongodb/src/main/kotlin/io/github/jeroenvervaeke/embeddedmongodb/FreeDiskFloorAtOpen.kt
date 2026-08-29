package io.github.jeroenvervaeke.embeddedmongodb

/**
 * Puts [requested] in force on a database that has just opened, or MongoDB's own floors where the
 * caller named none, closing the database if the engine refuses.
 *
 * ## Why an open sets a floor nobody asked for
 *
 * The two floors are server parameters, and this engine keeps one runtime for the whole life of
 * the process — `embedded_mongodb_initialize` runs from a namespace-scope initializer in
 * `cpp/bridge.cc`, so it happens at `System.loadLibrary` and never again. A floor therefore
 * belongs to the *process*, not to the [EmbeddedMongo] that named it, and outlives that
 * instance's [EmbeddedMongo.close]. A database that lowers the floor to 32 MiB and closes would
 * leave the next one — opened with no options at all, by a caller who asked for nothing — running
 * on 32 MiB. Silently, because the API names the floor per-[EmbeddedMongo.open] and nothing about
 * that suggests it is process-wide.
 *
 * That is worth more than a tidy test. This engine runs no `DiskSpaceMonitor`, and WiredTiger
 * answers a genuinely full disk with `WT_PANIC`, which MongoDB answers with `fassert` and a
 * process abort no caller can catch. The floor is the only warning an application gets before
 * that, so running on a floor someone else lowered is exactly the failure [FreeDiskFloor] exists
 * to prevent.
 *
 * ## Why here and not at close
 *
 * Putting the floor back in [EmbeddedMongo.close] would be symmetric with applying it at open,
 * and would be a guarantee nobody could rely on: Android kills processes, so a close that runs
 * only sometimes leaves the floor correct only sometimes. Establishing it at open holds however
 * the last database ended, and holds on the first open of a fresh process just the same.
 *
 * The cost is that an open naming no floor now depends on two more commands, so a MongoDB that
 * renamed a knob fails every open rather than only the opens that asked for a floor. That is the
 * right way round: a knob this library cannot find is a floor it cannot promise, and a loud
 * failure at open beats a silent wrong floor at the index build, on the device where the index
 * build was the thing that had to work.
 */
internal fun EmbeddedMongo.establishFreeDiskFloor(
    requested: FreeDiskFloor?,
    defaults: EngineFloorDefaults = EngineFloorDefaults.PROCESS,
): EmbeddedMongo {
    try {
        // Read unconditionally, and before anything is applied: the first open in a process may
        // well be one that names a floor, and recording afterwards would take that caller's floor
        // for MongoDB's and hand it to every later open that asked for the default.
        val engineOwn = defaults.of(this)
        if (requested == null) restoreFreeDiskFloorsBlocking(engineOwn)
        else setFreeDiskFloorBlocking(requested)
    } catch (failure: Throwable) {
        // Only one runtime may exist per process, so an open that fails after the engine started
        // has to take the engine with it -- an engine nobody holds a handle to is one this process
        // can never open a database in again.
        try {
            close()
        } catch (closing: Throwable) {
            failure.addSuppressed(closing)
        }
        throw failure
    }
    return this
}

/**
 * The free-disk floors MongoDB itself starts with, read from the engine once and remembered for
 * the life of the process.
 *
 * Read rather than written down as a constant. A constant would make this library the authority
 * on a number it does not own: a MongoDB whose default moved would be quietly overridden with the
 * old one on every open, and the test that pins the default would go on passing because it would
 * be checking this library's constant against itself rather than against the engine.
 *
 * The first open in the process is the only moment the defaults are knowable, and it is a reliable
 * one. The floors are server parameters, so nothing can have moved them before a database exists
 * to move them through, and [establishFreeDiskFloor] reads them before it applies anything and
 * before the caller who could move them is handed the database.
 *
 * Injected rather than reached for as a singleton, so that a test gets a fresh one: a floor
 * recorded by one test and read by the next is the very defect this exists to fix.
 */
internal class EngineFloorDefaults {
    @Volatile
    private var recorded: ReportedFloors? = null

    /**
     * What the floors were before anything moved them, asking [database] the first time only.
     *
     * @throws EmbeddedMongoException if the engine will not report its floors.
     */
    fun of(database: EmbeddedMongo): ReportedFloors =
        recorded ?: synchronized(this) {
            recorded ?: database.freeDiskFloorsBlocking().also { recorded = it }
        }

    companion object {
        /** The one every open but a test's reaches, because the engine behind it is one too. */
        val PROCESS: EngineFloorDefaults = EngineFloorDefaults()
    }
}

package io.github.jeroenvervaeke.embeddedmongodb

/**
 * How much room the engine may take, chosen when the database is opened.
 *
 * Every limit left `null` keeps the engine's own default, so `StorageOptions()` opens exactly
 * the way [EmbeddedMongo.open] without options opens. The defaults are already sized for a
 * phone rather than for a server — a directory holding 2.25 MiB of documents and indexes
 * occupies 10.25 MiB where mongod's journal settings would make it 202 MiB — so this is for an
 * application that knows something the library cannot:
 *
 * ```
 * // Inside a coroutine: open suspends, as it does without options.
 * val database = EmbeddedMongo.open(
 *     context,
 *     File(context.filesDir, "shop"),
 *     StorageOptions(
 *         cacheSize = CacheSize.ofMebibytes(32),
 *         freeDiskFloor = FreeDiskFloor.ofMebibytes(64),
 *     ),
 * )
 * ```
 *
 * The first three are read once while WiredTiger is opening and cannot be changed afterwards.
 * [freeDiskFloor] is a pair of server parameters, so it can also be moved on a database that is
 * already open — see [setFreeDiskFloor]. That split matters to this library and not to a
 * caller, which is why all four are named in one place.
 *
 * Being server parameters, the floors belong to the process rather than to one database, so
 * every [EmbeddedMongo.open] establishes them: naming no [freeDiskFloor] opens on MongoDB's own,
 * not on whatever a database closed earlier in this process left behind. [FreeDiskFloor] has the
 * whole of it. The first three carry no such history — they are given to WiredTiger as it opens.
 *
 * @property cacheSize the ceiling on the WiredTiger cache. Default 256 MB, which is a ceiling
 *   the engine grows into rather than memory it takes.
 * @property journalFileSize the size of one journal file, which every journal file is
 *   allocated at in full the moment it is created. Default 8 MiB, against mongod's 100 MB.
 * @property journalPreallocation whether a spare journal file is kept ready ahead of the one
 *   being written. Default off, which halves what an idle journal costs on disk and takes
 *   nothing away from durability: the file is created, extended and fsynced identically either
 *   way, only earlier.
 * @property freeDiskFloor how much free disk space an index build or a spilling query insists
 *   on before it starts. Default 500 MB, MongoDB's own. **Read [FreeDiskFloor] before lowering
 *   it.**
 */
data class StorageOptions(
    val cacheSize: CacheSize? = null,
    val journalFileSize: JournalFileSize? = null,
    val journalPreallocation: JournalPreallocation? = null,
    val freeDiskFloor: FreeDiskFloor? = null,
)

/**
 * The ceiling on the WiredTiger cache.
 *
 * A ceiling rather than an allocation: the engine grows into it as pages are read, so this
 * decides how much resident memory a busy engine may reach, not what an idle one costs.
 */
@JvmInline
value class CacheSize private constructor(val mebibytes: Int) {
    companion object {
        /** WiredTiger's `cache_size` is `min=1MB,max=10TB`, from its own `config_def.c`. */
        const val MIN_MEBIBYTES: Int = 1

        const val MAX_MEBIBYTES: Int = 10_000_000

        /**
         * @throws IllegalArgumentException if [mebibytes] is outside the range WiredTiger
         *   accepts. Checked here so that an unusable number is a mistake at the line that
         *   wrote it, rather than a failed open somewhere else.
         */
        fun ofMebibytes(mebibytes: Int): CacheSize =
            CacheSize(inRange("cache size", "MiB", mebibytes, MIN_MEBIBYTES, MAX_MEBIBYTES))
    }
}

/**
 * The size of one journal file.
 *
 * WiredTiger allocates each one in full the moment it creates it, so this is what an otherwise
 * empty database directory costs on disk. It does not bound what the journal costs under
 * sustained writing — files are removed once a checkpoint makes them obsolete — it bounds what
 * an idle database costs, which for an application that is mostly not being used is nearly all
 * of the time.
 */
@JvmInline
value class JournalFileSize private constructor(val kibibytes: Int) {
    companion object {
        /** WiredTiger's `log.file_max` is `min=100KB,max=2GB`, from the same `config_def.c`. */
        const val MIN_KIBIBYTES: Int = 100

        const val MAX_KIBIBYTES: Int = 2 * 1024 * 1024

        /**
         * @throws IllegalArgumentException if [kibibytes] is outside the range WiredTiger
         *   accepts. Going far below the 8 MiB default is worth measuring rather than
         *   assuming: under 2.5 MiB WiredTiger shrinks its log slot buffers to a tenth of this
         *   and starts pushing ordinary writes down its unbuffered path.
         */
        fun ofKibibytes(kibibytes: Int): JournalFileSize =
            JournalFileSize(inRange("journal file size", "KiB", kibibytes, MIN_KIBIBYTES, MAX_KIBIBYTES))
    }
}

/**
 * Whether WiredTiger keeps a spare journal file ready ahead of the one it is writing.
 *
 * The spare costs a second full-size journal file on disk at all times and buys the writing
 * thread the latency of creating one at a rollover. Durability does not enter into it.
 *
 * @property slot the value this policy takes in the option vector the native bridge reads.
 *   Zero is reserved there for "the caller named nothing", which is why these start at one.
 */
enum class JournalPreallocation(internal val slot: Long) {
    ENABLED(1),
    DISABLED(2),
}

/**
 * Checks one limit against its range.
 *
 * Shared so that every limit here reports a violation the same way and in the same words the
 * Rust and native layers use, which is what makes a message worth searching for.
 */
internal fun inRange(name: String, unit: String, value: Int, low: Int, high: Int): Int {
    require(value in low..high) { "$name must be between $low and $high $unit, got $value" }
    return value
}

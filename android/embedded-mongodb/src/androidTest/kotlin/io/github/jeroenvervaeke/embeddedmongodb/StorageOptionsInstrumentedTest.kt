package io.github.jeroenvervaeke.embeddedmongodb

import android.content.Context
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import java.io.File
import kotlin.test.assertEquals
import kotlin.test.assertFailsWith
import kotlin.test.assertTrue
import org.bson.Document
import org.junit.After
import org.junit.Test
import org.junit.runner.RunWith

/**
 * That the storage limits reach the engine on a device, over the ABI an application actually
 * loads.
 *
 * The JVM harnesses in the Rust crate prove the same two mechanisms against the host build, so
 * what is left for a device is that the new entry point is bound in the shared library the AAR
 * ships and that the Kotlin types encode what the engine reads.
 *
 * Only one engine may run in a process, so a database is opened per test and closed before the
 * next — and a test that wants two opens closes the first itself. The free-disk floors survive
 * that close, being server parameters rather than anything belonging to a database, which is why
 * no assertion here may assume it inherited a clean one.
 */
@RunWith(AndroidJUnit4::class)
class StorageOptionsInstrumentedTest {
    private val context: Context = InstrumentationRegistry.getInstrumentation().targetContext
    private val root = File(context.filesDir, "embedded-mongodb-options-${System.nanoTime()}")
    private var database: EmbeddedMongo? = null

    @After
    fun closeDatabase() {
        database?.close()
        root.deleteRecursively()
    }

    @Test
    fun everyLimitReachesWiredTiger() {
        val options = StorageOptions(
            cacheSize = CacheSize.ofMebibytes(64),
            journalFileSize = JournalFileSize.ofKibibytes(512),
            journalPreallocation = JournalPreallocation.ENABLED,
        )

        val wiredTiger =
            open(options).runCommandBlocking("admin", Document("serverStatus", 1)).stats("wiredTiger")

        // Both are published while the connection opens, so they are settled by now.
        assertEquals(64L * 1024 * 1024, wiredTiger.stats("cache").stat("maximum bytes configured"))
        assertEquals(512L * 1024, wiredTiger.stats("log").stat("maximum log file size"))
        // Pre-allocation is not: WiredTiger publishes this count from the log server thread, so it
        // reads 0 until that thread's first pass, and rises above one when the writing thread has
        // had to allocate a file itself.
        assertTrue(preallocatedFiles() >= 1, "journal pre-allocation never reached WiredTiger")
    }

    /** A caller who names one limit must not have the others chosen for them. */
    @Test
    fun theLimitsNobodyNamedStayTheEnginesOwn() {
        val options = StorageOptions(cacheSize = CacheSize.ofMebibytes(64))

        val wiredTiger =
            open(options).runCommandBlocking("admin", Document("serverStatus", 1)).stats("wiredTiger")

        assertEquals(64L * 1024 * 1024, wiredTiger.stats("cache").stat("maximum bytes configured"))
        assertEquals(8L * 1024 * 1024, wiredTiger.stats("log").stat("maximum log file size"))
    }

    @Test
    fun aDatabaseOpenedWithoutOptionsRunsOnMongoDbsOwnFloor() {
        val floors = open(StorageOptions()).freeDiskFloorsBlocking()

        assertEquals(ReportedFloors(500L, 500L * 1024 * 1024), floors)
    }

    /**
     * The floors are process-global server parameters, so one this database lowered is still in
     * force after its close — and the next open inherits it unless the open puts it back.
     *
     * This is what [aDatabaseOpenedWithoutOptionsRunsOnMongoDbsOwnFloor] cannot pin on its own:
     * that one only meets a dirtied floor when the runner happens to schedule it after a test
     * that lowered one, so it caught this defect on some orderings and not others. Here the dirty
     * floor is the arrangement rather than an accident of ordering, so the assertion holds
     * whatever runs before it and fails whenever the open stops establishing the floor.
     */
    @Test
    fun aFloorLeftBehindByAClosedDatabaseDoesNotReachTheNextOne() {
        val lowered = open(StorageOptions(freeDiskFloor = FreeDiskFloor.ofMebibytes(32)), "lowered")
        assertEquals(
            ReportedFloors(32L, 32L * 1024 * 1024),
            lowered.freeDiskFloorsBlocking(),
            "the floor to be left behind was never lowered, so this proves nothing",
        )
        lowered.close()

        val floors = open(StorageOptions(), "inheriting").freeDiskFloorsBlocking()

        assertEquals(ReportedFloors(500L, 500L * 1024 * 1024), floors)
    }

    @Test
    fun theFloorNamedAtOpenIsInForceBeforeTheCallerGetsTheDatabase() {
        val options = StorageOptions(freeDiskFloor = FreeDiskFloor.ofMebibytes(64))

        val floors = open(options).freeDiskFloorsBlocking()

        assertEquals(ReportedFloors(64L, 64L * 1024 * 1024), floors)
    }

    @Test
    fun theFloorCanBeMovedOnARunningDatabase() {
        val opened = open(StorageOptions())

        opened.setFreeDiskFloorBlocking(FreeDiskFloor.ofMebibytes(32))

        assertEquals(ReportedFloors(32L, 32L * 1024 * 1024), opened.freeDiskFloorsBlocking())
    }

    /**
     * The case the whole knob exists for: an application seeding on first launch, on a device
     * without the 500 MB MongoDB asks for. Driven from the wrong end, because asking for more
     * free space than the device has is the only way to watch the check fire — and it fires for
     * exactly the reason a nearly-full phone would.
     */
    @Test
    fun anIndexBuildRefusedByTheFloorRunsOnceTheFloorIsLowered() {
        val unreachable = FreeDiskFloor.ofMebibytes(4 * 1024 * 1024)
        val opened = open(StorageOptions(freeDiskFloor = unreachable))
        opened.runCommandBlocking(
            DATABASE,
            Document("insert", COLLECTION).append("documents", listOf(Document("_id", 1))),
        )

        val refused = assertFailsWith<EmbeddedMongoException> {
            opened.runCommandBlocking(DATABASE, createIndex())
        }
        assertEquals(OUT_OF_DISK_SPACE, refused.code)

        opened.setFreeDiskFloorBlocking(FreeDiskFloor.ofMebibytes(32))
        assertEquals(1.0, opened.runCommandBlocking(DATABASE, createIndex()).getDouble("ok"))
    }

    /** Polls until the log server thread has published the count, or gives up. */
    private fun preallocatedFiles(): Long {
        val deadline = System.nanoTime() + PATIENCE_NANOS
        var reported = 0L
        while (System.nanoTime() < deadline) {
            val database = requireNotNull(database) { "the database must be open" }
            reported = database.runCommandBlocking("admin", Document("serverStatus", 1))
                .stats("wiredTiger").stats("log").stat("number of pre-allocated log files to create")
            if (reported >= 1) return reported
            Thread.sleep(POLL_MILLIS)
        }
        return reported
    }

    /**
     * [directory] is named so that a test opening twice gets two databases rather than a reopen
     * of one, which would put a second thing on trial alongside the floor.
     */
    private fun open(options: StorageOptions, directory: String = "main"): EmbeddedMongo =
        EmbeddedMongo.openBlocking(context, File(root, directory), options).also { database = it }

    private fun createIndex(): Document = Document("createIndexes", COLLECTION)
        .append("indexes", listOf(Document("key", Document("name", 1)).append("name", "name_1")))
}

private fun Document.stats(category: String): Document = get(category, Document::class.java)

/**
 * WiredTiger's statistics are appended with `appendNumber`, which picks int32 or int64 by what
 * the value happens to need, so reading one as either type would break on the other.
 */
private fun Document.stat(name: String): Long = (this[name] as Number).toLong()

private const val DATABASE = "shop"
private const val COLLECTION = "orders"

/** `ErrorCodes::OutOfDiskSpace`, from src/mongo/base/error_codes.yml. */
private const val OUT_OF_DISK_SPACE = 14031

/** Long enough that a loaded device cannot mistake a slow log server thread for a failure. */
private const val PATIENCE_NANOS = 60L * 1000 * 1000 * 1000

private const val POLL_MILLIS = 50L

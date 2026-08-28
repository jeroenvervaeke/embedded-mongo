package io.github.jeroenvervaeke.embeddedmongodb

import android.content.Context
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import java.io.File
import kotlin.test.assertEquals
import kotlin.test.assertFailsWith
import kotlin.test.assertNotNull
import kotlin.test.assertTrue
import kotlinx.coroutines.flow.count
import kotlinx.coroutines.runBlocking
import org.bson.Document
import org.junit.After
import org.junit.Before
import org.junit.Test
import org.junit.runner.RunWith

/**
 * The half of the library that only a device can answer for: that the two shared libraries load,
 * that the engine runs, and that the main thread guard sees Android's real Looper.
 *
 * The document count is deliberately larger than a single MongoDB batch, so the `getMore` paging
 * is exercised against the engine rather than against a fake.
 */
@RunWith(AndroidJUnit4::class)
class EmbeddedMongoInstrumentedTest {
    private val context: Context = InstrumentationRegistry.getInstrumentation().targetContext
    private lateinit var root: File
    private lateinit var directory: File
    private lateinit var database: EmbeddedMongo

    @Before
    fun openDatabase() {
        root = File(context.filesDir, "embedded-mongodb-${System.nanoTime()}")
        directory = File(root, "main")
        database = EmbeddedMongo.openBlocking(directory)
    }

    @After
    fun closeDatabase() {
        database.close()
        root.deleteRecursively()
    }

    @Test
    fun theEngineAnswersCommands() {
        val reply = database.commandBlocking(DATABASE, Document("ping", 1))

        assertEquals(1.0, reply.getDouble("ok"))
    }

    @Test
    fun everyDocumentArrivesHoweverManyBatchesItTakes() {
        insertOrders(ORDERS)

        val read = database.cursor(DATABASE, Document("find", COLLECTION)).use { it.count() }

        assertEquals(ORDERS, read)
    }

    @Test
    fun theFlowEmitsEveryDocument() {
        insertOrders(ORDERS)

        val read = runBlocking { database.documents(DATABASE, Document("find", COLLECTION)).count() }

        assertEquals(ORDERS, read)
    }

    @Test
    fun abandoningACursorLeavesTheDatabaseUsable() {
        insertOrders(ORDERS)

        val read = database.cursor(DATABASE, Document("find", COLLECTION)).use { it.take(5).toList() }

        assertEquals(5, read.size)
        assertEquals(ORDERS, database.cursor(DATABASE, Document("find", COLLECTION)).use { it.count() })
    }

    @Test
    fun aRejectedCommandCarriesTheServerErrorCode() {
        val failure = assertFailsWith<EmbeddedMongoException> {
            database.commandBlocking(DATABASE, Document("thereIsNoSuchCommand", 1))
        }

        assertEquals(COMMAND_NOT_FOUND, failure.code)
    }

    @Test
    fun aDuplicateKeyIsRaisedEvenThoughTheCommandItselfSucceeds() {
        val order = Document("_id", 1)
        val insert = Document("insert", COLLECTION).append("documents", listOf(order))
        database.commandBlocking(DATABASE, insert)

        val failure = assertFailsWith<EmbeddedMongoException> { database.commandBlocking(DATABASE, insert) }

        assertEquals(DUPLICATE_KEY, failure.code)
    }

    @Test
    fun blockingCallsAreRefusedOnTheMainThread() {
        var failure: Throwable? = null

        InstrumentationRegistry.getInstrumentation().runOnMainSync {
            failure = assertFailsWith<IllegalStateException> {
                database.commandBlocking(DATABASE, Document("ping", 1))
            }
        }

        assertTrue(failure?.message.orEmpty().contains("main thread"))
    }

    @Test
    fun theSuspendingApiIsUsableFromTheMainThread() {
        var reply: Document? = null

        InstrumentationRegistry.getInstrumentation().runOnMainSync {
            reply = runBlocking { database.command(DATABASE, Document("ping", 1)) }
        }

        assertEquals(1.0, reply?.getDouble("ok"))
    }

    @Test
    fun theSuspendingOpenReturnsAUsableDatabase() {
        database.close()

        database = runBlocking { EmbeddedMongo.open(directory) }

        assertEquals(1.0, database.commandBlocking(DATABASE, Document("ping", 1)).getDouble("ok"))
    }

    @Test
    fun openingBlockingOnTheMainThreadIsRefused() {
        var failure: Throwable? = null

        InstrumentationRegistry.getInstrumentation().runOnMainSync {
            failure = assertFailsWith<IllegalStateException> { EmbeddedMongo.openBlocking(directory) }
        }

        assertTrue(failure?.message.orEmpty().contains("main thread"))
    }

    @Test
    fun documentsSurviveClosingAndReopening() {
        insertOrders(ORDERS)
        database.close()

        database = EmbeddedMongo.openBlocking(directory)

        assertEquals(ORDERS, database.cursor(DATABASE, Document("find", COLLECTION)).use { it.count() })
    }

    @Test
    fun aSecondDatabaseInTheSameProcessIsRefused() {
        val failure = assertFailsWith<EmbeddedMongoException> {
            EmbeddedMongo.openBlocking(File(root, "second"))
        }

        assertTrue(failure.message.orEmpty().contains("one embedded MongoDB runtime"))
    }

    @Test
    fun aClosedEngineRefusesFurtherCommands() {
        // The engine allows one runtime per process, so the database this test opens has to be
        // the only one.
        database.close()
        val engine = NativeEngine.open(File(root, "closed").apply { mkdirs() })
        engine.close()

        assertFailsWith<IllegalStateException> {
            engine.command(DATABASE, BsonCodec.encode(Document("ping", 1)))
        }
    }

    @Test
    fun closingTheEngineTwiceDoesNotReachTheBridgeTwice() {
        database.close()
        val engine = NativeEngine.open(File(root, "twice").apply { mkdirs() })

        engine.close()

        // The bridge rejects a handle it already released, so a second close reaching it would
        // throw. Silence here is the guard in NativeEngine doing its job.
        engine.close()
    }

    @Test
    fun thePlatformReportsHowMuchRoomItCanGiveTheEngine() {
        val allocatable = allocatableBytes(context, root)

        // The API this rests on arrived in API 26 and this runs above it, so a null here would
        // mean the measurement quietly does nothing on the devices it was written for.
        assertNotNull(allocatable)
        assertTrue(allocatable > 0, "the platform reported $allocatable allocatable bytes")
    }

    @Test
    fun openingWithAContextChecksTheVolumeAndThenOpens() {
        database.close()

        database = EmbeddedMongo.openBlocking(context, File(root, "checked"))

        assertEquals(1.0, database.commandBlocking(DATABASE, Document("ping", 1)).getDouble("ok"))
    }

    @Test
    fun openingSomethingThatIsNotADirectoryIsRefused() {
        database.close()
        val file = File(root, "a-file").apply { writeText("not a database") }

        assertFailsWith<IllegalArgumentException> { EmbeddedMongo.openBlocking(file) }
    }

    private fun insertOrders(count: Int) {
        val orders = (1..count).map { Document("_id", it).append("value", "order $it") }
        database.commandBlocking(DATABASE, Document("insert", COLLECTION).append("documents", orders))
    }
}

private const val DATABASE = "shop"
private const val COLLECTION = "orders"

/** More than the 101 documents a first batch holds, so the cursor has to ask for more. */
private const val ORDERS = 5000

private const val COMMAND_NOT_FOUND = 59
private const val DUPLICATE_KEY = 11000

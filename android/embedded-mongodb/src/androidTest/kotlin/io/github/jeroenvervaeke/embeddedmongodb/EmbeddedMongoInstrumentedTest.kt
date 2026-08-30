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
import kotlinx.coroutines.flow.take
import kotlinx.coroutines.flow.toList
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
 * The collection API is exercised here rather than only against a fake, because what a fake
 * cannot check is that MongoDB accepts the commands it builds. The document count is deliberately
 * larger than a single MongoDB batch, so the `getMore` paging runs against the engine.
 */
@RunWith(AndroidJUnit4::class)
class EmbeddedMongoInstrumentedTest {
    private val context: Context = InstrumentationRegistry.getInstrumentation().targetContext
    private lateinit var root: File
    private lateinit var directory: File
    private lateinit var mongo: EmbeddedMongo

    private val orders: MongoCollection get() = mongo.getDatabase(DATABASE).getCollection(COLLECTION)

    @Before
    fun openDatabase() {
        root = File(context.filesDir, "embedded-mongodb-${System.nanoTime()}")
        directory = File(root, "main")
        mongo = EmbeddedMongo.openBlocking(directory)
    }

    @After
    fun closeDatabase() {
        mongo.close()
        root.deleteRecursively()
    }

    @Test
    fun theEngineAnswersCommands() {
        val reply = mongo.runCommandBlocking(DATABASE, Document("ping", 1))

        assertEquals(1.0, reply.getDouble("ok"))
    }

    @Test
    fun everyDocumentArrivesHoweverManyBatchesItTakes() = runBlocking {
        insertOrders(ORDERS)

        val read = orders.find().asFlow().count()

        assertEquals(ORDERS, read)
    }

    @Test
    fun abandoningACursorLeavesTheDatabaseUsable() = runBlocking {
        insertOrders(ORDERS)

        val read = orders.find().asFlow().take(5).toList()

        assertEquals(5, read.size)
        assertEquals(ORDERS, orders.find().asFlow().count())
    }

    @Test
    fun theEngineAcceptsTheCommandsTheCollectionApiBuilds() = runBlocking {
        val inserted = orders.insertMany(
            listOf(Document("value", "first"), Document("value", "second")),
        )

        assertEquals(2, inserted.insertedIds.size)
        assertEquals(2L, orders.countDocuments())
        assertEquals(1L, orders.countDocuments(Document("value", "first")))
        // The ids are keyed by the position of the document that got them.
        assertEquals(
            "first",
            orders.find(Document("_id", inserted.insertedIds.getValue(0))).firstOrNull()?.getString("value"),
        )
    }

    @Test
    fun anUpdateAndADeleteReportWhatTheyReached() = runBlocking {
        orders.insertMany(listOf(Document("paid", false), Document("paid", false)))

        val updated = orders.updateMany(Document("paid", false), Document("\$set", Document("paid", true)))
        val deleted = orders.deleteOne(Document("paid", true))

        assertEquals(UpdateResult(matchedCount = 2, modifiedCount = 2), updated)
        assertEquals(DeleteResult(1), deleted)
        assertEquals(1L, orders.countDocuments())
    }

    @Test
    fun anIndexIsBuiltAndReportedByTheNameItWasBuiltUnder() = runBlocking {
        orders.insertOne(Document("customer", "ada"))

        val name = orders.createIndex(Indexes.ascending("customer"))

        assertEquals("customer_1", name)
        assertTrue(orders.listIndexes().any { it.getString("name") == name }, "${orders.listIndexes()}")
    }

    @Test
    fun anAggregationRunsInTheEngineAndPagesItsCursor() = runBlocking {
        insertOrders(ORDERS)

        val grouped = orders.aggregate(
            Document("\$group", Document("_id", null).append("total", Document("\$sum", 1))),
        ).toList()

        assertEquals(ORDERS, grouped.single().getInteger("total"))
    }

    @Test
    fun droppingACollectionThatIsNotThereIsTheStateTheCallerAskedFor() = runBlocking {
        // A collection nothing has written to does not exist, and MongoDB reports dropping one as
        // a failure. `drop` reads that code and answers the question that was asked, as the
        // driver does.
        mongo.getDatabase(DATABASE).getCollection("never-written").drop()
    }

    @Test
    fun droppingAnIndexReportsWhatTheEngineReports() = runBlocking {
        // Measured here rather than taken from the driver's documentation, which disagrees: the
        // driver raises IndexNotFound for a name that is not there, and this engine does not
        // treat that as a failure at all. Only a device can settle that -- the JVM test beside
        // this one answers with whichever code its fake was handed.
        val neverWritten = mongo.getDatabase(DATABASE).getCollection("never-written")

        // A collection that does not exist: the namespace is missing, and the engine says so
        // before it looks for an index inside it.
        val missingCollection = assertFailsWith<EmbeddedMongoException> {
            neverWritten.dropIndex("no_such_index")
        }
        assertEquals(MongoErrorCode.NAMESPACE_NOT_FOUND, missingCollection.code)

        orders.insertOne(Document("value", "first"))

        // An index that does not exist on a collection that does: not an error here.
        orders.dropIndex("no_such_index")

        // `_id_` cannot be dropped at all, and that is a failure of its own rather than a no-op.
        assertFailsWith<EmbeddedMongoException> { orders.dropIndex("_id_") }
    }

    @Test
    fun anUnorderedInsertReportsEveryDocumentItRejected() = runBlocking {
        orders.insertMany(listOf(Document("_id", 1), Document("_id", 2)))

        val failure = assertFailsWith<EmbeddedMongoException> {
            orders.insertMany(
                listOf(Document("_id", 1), Document("_id", 3), Document("_id", 2)),
                ordered = false,
            )
        }

        // Two rejections rather than the first, and the good document went in regardless.
        assertEquals(2, failure.writeErrors.size, "${failure.writeErrors}")
        assertEquals(MongoErrorCode.DUPLICATE_KEY, failure.code)
        assertEquals(1L, failure.response?.let { (it["n"] as Number).toLong() })
        assertEquals(3L, orders.countDocuments())
    }

    @Test
    fun aRejectedCommandCarriesTheServerErrorCode() {
        val failure = assertFailsWith<EmbeddedMongoException> {
            mongo.runCommandBlocking(DATABASE, Document("thereIsNoSuchCommand", 1))
        }

        assertEquals(COMMAND_NOT_FOUND, failure.code)
    }

    @Test
    fun aDuplicateKeyIsRaisedEvenThoughTheCommandItselfSucceeds() = runBlocking {
        val order = Document("_id", 1)
        orders.insertOne(order)

        val failure = assertFailsWith<EmbeddedMongoException> { orders.insertOne(order) }

        assertEquals(MongoErrorCode.DUPLICATE_KEY, failure.code)
    }

    @Test
    fun blockingCallsAreRefusedOnTheMainThread() {
        var failure: Throwable? = null

        InstrumentationRegistry.getInstrumentation().runOnMainSync {
            failure = assertFailsWith<IllegalStateException> {
                mongo.runCommandBlocking(DATABASE, Document("ping", 1))
            }
        }

        assertTrue(failure?.message.orEmpty().contains("main thread"))
    }

    @Test
    fun theSuspendingApiIsUsableFromTheMainThread() {
        var reply: Document? = null

        InstrumentationRegistry.getInstrumentation().runOnMainSync {
            reply = runBlocking { mongo.runCommand(DATABASE, Document("ping", 1)) }
        }

        assertEquals(1.0, reply?.getDouble("ok"))
    }

    @Test
    fun theSuspendingOpenReturnsAUsableDatabase() {
        mongo.close()

        mongo = runBlocking { EmbeddedMongo.open(directory) }

        assertEquals(1.0, mongo.runCommandBlocking(DATABASE, Document("ping", 1)).getDouble("ok"))
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
    fun documentsSurviveClosingAndReopening() = runBlocking {
        insertOrders(ORDERS)
        mongo.close()

        mongo = EmbeddedMongo.openBlocking(directory)

        assertEquals(ORDERS.toLong(), orders.countDocuments())
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
        mongo.close()
        val engine = NativeEngine.open(File(root, "closed").apply { mkdirs() })
        engine.close()

        assertFailsWith<IllegalStateException> {
            engine.command(DATABASE, BsonCodec.encode(Document("ping", 1)))
        }
    }

    @Test
    fun closingTheEngineTwiceDoesNotReachTheBridgeTwice() {
        mongo.close()
        val engine = NativeEngine.open(File(root, "twice").apply { mkdirs() })

        engine.close()

        // The bridge rejects a handle it already released, so a second close reaching it would
        // throw. Silence here is the guard in NativeEngine doing its job.
        engine.close()
    }

    @Test
    fun thePlatformReportsHowMuchRoomItCanGiveTheEngine() {
        val allocatable = allocatableBytes(context, root)

        // The API this rests on arrived in API 26, which is this library's floor, so it is there
        // on every supported device: a null here would mean the measurement quietly does nothing.
        assertNotNull(allocatable)
        assertTrue(allocatable > 0, "the platform reported $allocatable allocatable bytes")
    }

    @Test
    fun openingWithAContextChecksTheVolumeAndThenOpens() {
        mongo.close()

        mongo = EmbeddedMongo.openBlocking(context, File(root, "checked"))

        assertEquals(1.0, mongo.runCommandBlocking(DATABASE, Document("ping", 1)).getDouble("ok"))
    }

    @Test
    fun openingSomethingThatIsNotADirectoryIsRefused() {
        mongo.close()
        val file = File(root, "a-file").apply { writeText("not a database") }

        assertFailsWith<IllegalArgumentException> { EmbeddedMongo.openBlocking(file) }
    }

    private fun insertOrders(count: Int) {
        val documents = (1..count).map { Document("_id", it).append("value", "order $it") }
        mongo.runCommandBlocking(DATABASE, Document("insert", COLLECTION).append("documents", documents))
    }
}

private const val DATABASE = "shop"
private const val COLLECTION = "orders"

/** More than the 101 documents a first batch holds, so the cursor has to ask for more. */
private const val ORDERS = 5000

private const val COMMAND_NOT_FOUND = 59

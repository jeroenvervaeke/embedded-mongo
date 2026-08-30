package io.github.jeroenvervaeke.embeddedmongodb

import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFailsWith
import kotlin.test.assertNull
import kotlinx.coroutines.test.runTest
import org.bson.BsonDocument
import org.bson.BsonInt32
import org.bson.Document

class FindQueryTest {
    @Test
    fun `a bare find names only the collection`() {
        assertEquals(Document("find", "orders"), FakeMongo().orders.find().command())
    }

    @Test
    fun `every option the caller named reaches the command`() {
        val command = FakeMongo().orders.find(Document("paid", true))
            .sort(Document("placed", -1))
            .projection(Document("total", 1))
            .skip(10)
            .limit(5)
            .batchSize(2)
            .command()

        assertEquals(
            Document("find", "orders")
                .append("filter", Document("paid", true))
                .append("sort", Document("placed", -1))
                .append("projection", Document("total", 1))
                .append("skip", 10)
                .append("limit", 5)
                .append("batchSize", 2),
            command,
        )
    }

    @Test
    fun `narrowing a query leaves the one it was narrowed from alone`() {
        val paid = FakeMongo().orders.find(Document("paid", true))

        val limited = paid.limit(5)

        assertEquals(Document("find", "orders").append("filter", Document("paid", true)), paid.command())
        assertEquals(5, limited.command()["limit"])
    }

    @Test
    fun `a filter built by something other than Document still reaches the command`() {
        // Which is the whole reason these take org.bson.conversions.Bson: the official driver's
        // Filters returns one of these rather than a Document.
        val filter = BsonDocument("total", BsonInt32(12))

        val command = FakeMongo().orders.find(filter).command()

        assertEquals(Document("total", 12), command["filter"])
    }

    @Test
    fun `collecting sends the command and returns what the engine answered`() = runTest {
        val mongo = FakeMongo { singleBatch(documents(1..3)) }

        val read = mongo.orders.find(Document("paid", true)).toList()

        assertEquals(documents(1..3), read)
        assertEquals(listOf(FakeMongo.DATABASE), mongo.commands.databases)
    }

    @Test
    fun `the first match is asked for as a limit of one`() = runTest {
        val mongo = FakeMongo { singleBatch(documents(1..1)) }

        val first = mongo.orders.find(Document("paid", true)).limit(500).firstOrNull()

        assertEquals(Document("n", 1), first)
        assertEquals(1, mongo.lastCommand["limit"])
    }

    @Test
    fun `a first match that is not there is null rather than a failure`() = runTest {
        val mongo = FakeMongo { singleBatch(emptyList()) }

        assertNull(mongo.orders.find().firstOrNull())
    }

    @Test
    fun `a limit of zero is MongoDB's spelling of no limit, and a negative one is rejected`() {
        val orders = FakeMongo().orders

        assertEquals(0, orders.find().limit(0).command()["limit"])
        assertFailsWith<IllegalArgumentException> { orders.find().limit(-1) }
    }

    @Test
    fun `a skip cannot be negative and a batch cannot be empty`() {
        val orders = FakeMongo().orders

        assertFailsWith<IllegalArgumentException> { orders.find().skip(-1) }
        assertFailsWith<IllegalArgumentException> { orders.find().batchSize(0) }
    }
}

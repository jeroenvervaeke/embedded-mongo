package io.github.jeroenvervaeke.embeddedmongodb

import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFailsWith
import kotlin.test.assertNull
import kotlinx.coroutines.test.runTest
import org.bson.Document

class AggregateQueryTest {
    @Test
    fun `a pipeline reaches the engine with the cursor options it needs`() {
        val match = Document("\$match", Document("paid", true))

        val command = FakeMongo().orders.aggregate(match).command()

        assertEquals(
            Document("aggregate", "orders")
                .append("pipeline", listOf(match))
                .append("cursor", Document()),
            command,
        )
    }

    @Test
    fun `stages can be added to a pipeline that was built elsewhere`() {
        val paid = FakeMongo().orders.aggregate(Document("\$match", Document("paid", true)))

        val limited = paid.then(Document("\$limit", 5))

        assertEquals(1, paid.command().pipeline().size)
        assertEquals(
            listOf(Document("\$match", Document("paid", true)), Document("\$limit", 5)),
            limited.command().pipeline(),
        )
    }

    @Test
    fun `a batch size is named inside the cursor options, where MongoDB takes it`() {
        val command = FakeMongo().orders.aggregate(Document("\$limit", 1)).batchSize(50).command()

        assertEquals(Document("batchSize", 50), command["cursor"])
    }

    @Test
    fun `spilling to disk is only named when the caller decided about it`() {
        val orders = FakeMongo().orders
        val pipeline = listOf(Document("\$limit", 1))

        assertEquals(null, orders.aggregate(pipeline).command()["allowDiskUse"])
        assertEquals(true, orders.aggregate(pipeline).allowDiskUse(true).command()["allowDiskUse"])
        assertEquals(false, orders.aggregate(pipeline).allowDiskUse(false).command()["allowDiskUse"])
    }

    @Test
    fun `an empty batch size is rejected before it reaches the engine`() {
        assertFailsWith<IllegalArgumentException> {
            FakeMongo().orders.aggregate(Document("\$limit", 1)).batchSize(0)
        }
    }

    @Test
    fun `collecting pages the cursor the pipeline opened`() = runTest {
        val mongo = FakeMongo { command ->
            if (command.containsKey("getMore")) cursorReply(0, "nextBatch", documents(3..3))
            else cursorReply(9, "firstBatch", documents(1..2))
        }

        val read = mongo.orders.aggregate(Document("\$limit", 3)).toList()

        assertEquals(documents(1..3), read)
    }

    @Test
    fun `a pipeline that produced no row at all answers null rather than failing`() = runTest {
        // Which is what a `$count` over an empty collection does: no row, not a row holding zero.
        val mongo = FakeMongo { singleBatch(emptyList()) }

        assertNull(mongo.orders.aggregate(Document("\$count", "count")).firstOrNull())
    }
}

@Suppress("UNCHECKED_CAST")
internal fun Document.pipeline(): List<Document> = this["pipeline"] as List<Document>

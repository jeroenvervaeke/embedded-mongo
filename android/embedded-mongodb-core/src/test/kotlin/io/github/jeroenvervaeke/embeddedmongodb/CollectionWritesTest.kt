package io.github.jeroenvervaeke.embeddedmongodb

import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFailsWith
import kotlin.test.assertNull
import kotlin.test.assertTrue
import kotlinx.coroutines.test.runTest
import org.bson.Document
import org.bson.types.ObjectId

class CollectionWritesTest {
    @Test
    fun `an insert without an id is given one, and the caller is told what it is`() = runTest {
        val mongo = FakeMongo { okReply("n" to 1) }

        val result = mongo.orders.insertOne(Document("total", 12))

        assertTrue(result.insertedId is ObjectId, "${result.insertedId}")
        assertEquals(result.insertedId, mongo.lastCommand.documents().single()["_id"])
    }

    @Test
    fun `an id the caller chose is kept rather than replaced`() = runTest {
        val mongo = FakeMongo { okReply("n" to 1) }

        val result = mongo.orders.insertOne(Document("_id", "first").append("total", 12))

        assertEquals("first", result.insertedId)
    }

    @Test
    fun `the caller's document is not the one that was given an id`() = runTest {
        // Otherwise inserting the same document twice would fail on a duplicate key the second
        // time, having quietly written an id into a document the caller still holds.
        val mongo = FakeMongo { okReply("n" to 1) }
        val order = Document("total", 12)

        mongo.orders.insertOne(order)

        assertNull(order["_id"])
    }

    @Test
    fun `a batch is stored in one command and every id comes back in order`() = runTest {
        val mongo = FakeMongo { okReply("n" to 2) }

        val result = mongo.orders.insertMany(
            listOf(Document("_id", "first"), Document("_id", "second")),
            ordered = false,
        )

        assertEquals(listOf("first", "second"), result.insertedIds)
        assertEquals(false, mongo.lastCommand["ordered"])
        assertEquals(1, mongo.sent.size)
    }

    @Test
    fun `an insert of nothing is refused here rather than by the engine`() = runTest {
        assertFailsWith<IllegalArgumentException> { FakeMongo().orders.insertMany(emptyList()) }
    }

    @Test
    fun `an engine that stored fewer documents than it was given is a failure`() = runTest {
        val mongo = FakeMongo { okReply("n" to 1) }

        val failure = assertFailsWith<EmbeddedMongoException> {
            mongo.orders.insertMany(listOf(Document("a", 1), Document("b", 2)))
        }

        assertTrue(failure.message!!.contains("stored 1 of 2"), failure.message!!)
    }

    @Test
    fun `an update names the filter, the change and how far it reaches`() = runTest {
        val mongo = FakeMongo { okReply("n" to 3, "nModified" to 2) }

        val result = mongo.orders.updateMany(
            Document("paid", false),
            Document("\$set", Document("paid", true)),
        )

        assertEquals(UpdateResult(matchedCount = 3, modifiedCount = 2), result)
        assertEquals(
            Document("q", Document("paid", false))
                .append("u", Document("\$set", Document("paid", true)))
                .append("multi", true)
                .append("upsert", false),
            mongo.lastCommand.updates().single(),
        )
    }

    @Test
    fun `updateOne reaches one document`() = runTest {
        val mongo = FakeMongo { okReply("n" to 1, "nModified" to 1) }

        mongo.orders.updateOne(Document("_id", "first"), Document("\$inc", Document("total", 1)))

        assertEquals(false, mongo.lastCommand.updates().single()["multi"])
    }

    @Test
    fun `an upsert reports what it created`() = runTest {
        val mongo = FakeMongo {
            okReply("n" to 1, "nModified" to 0, "upserted" to listOf(Document("index", 0).append("_id", "new")))
        }

        val result = mongo.orders.updateOne(
            Document("_id", "new"),
            Document("\$set", Document("total", 1)),
            upsert = true,
        )

        assertEquals("new", result.upsertedId)
        assertEquals(true, mongo.lastCommand.updates().single()["upsert"])
    }

    @Test
    fun `an update that matched nothing reports zero rather than failing on a missing nModified`() =
        runTest {
            val mongo = FakeMongo { okReply("n" to 0) }

            assertEquals(
                UpdateResult(matchedCount = 0, modifiedCount = 0),
                mongo.orders.updateOne(Document("_id", "gone"), Document("\$set", Document("a", 1))),
            )
        }

    @Test
    fun `a replacement holding update operators is refused, because it would be stored as fields`() =
        runTest {
            val mongo = FakeMongo { okReply("n" to 1, "nModified" to 1) }

            assertFailsWith<IllegalArgumentException> {
                mongo.orders.replaceOne(Document("_id", "first"), Document("\$set", Document("a", 1)))
            }
        }

    @Test
    fun `a replacement reaches the engine as the whole document it is`() = runTest {
        val mongo = FakeMongo { okReply("n" to 1, "nModified" to 1) }

        mongo.orders.replaceOne(Document("_id", "first"), Document("total", 20))

        assertEquals(Document("total", 20), mongo.lastCommand.updates().single()["u"])
    }

    @Test
    fun `deleting one match and deleting every match differ by MongoDB's own limit`() = runTest {
        val mongo = FakeMongo { okReply("n" to 4) }

        assertEquals(DeleteResult(4), mongo.orders.deleteMany(Document("paid", true)))
        assertEquals(0, mongo.lastCommand.deletes().single()["limit"])

        mongo.orders.deleteOne(Document("paid", true))
        assertEquals(1, mongo.lastCommand.deletes().single()["limit"])
    }
}

@Suppress("UNCHECKED_CAST")
private fun Document.documents(): List<Document> = this["documents"] as List<Document>

@Suppress("UNCHECKED_CAST")
private fun Document.updates(): List<Document> = this["updates"] as List<Document>

@Suppress("UNCHECKED_CAST")
private fun Document.deletes(): List<Document> = this["deletes"] as List<Document>

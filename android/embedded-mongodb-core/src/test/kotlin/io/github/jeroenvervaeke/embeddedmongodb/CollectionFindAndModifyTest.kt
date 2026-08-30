package io.github.jeroenvervaeke.embeddedmongodb

import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFailsWith
import kotlin.test.assertNull
import kotlinx.coroutines.test.runTest
import org.bson.Document

class CollectionFindAndModifyTest {
    @Test
    fun `an update in place hands back the document and changes it in one command`() = runTest {
        val claimed = Document("_id", 1).append("sent", false)
        val mongo = FakeMongo { okReply("value" to claimed) }

        val found = mongo.orders.findOneAndUpdate(
            Document("sent", false),
            Document("\$set", Document("sent", true)),
        )

        assertEquals(claimed, found)
        assertEquals(
            Document("findAndModify", "orders")
                .append("query", Document("sent", false))
                .append("update", Document("\$set", Document("sent", true)))
                .append("upsert", false)
                .append("new", false),
            mongo.lastCommand,
        )
        assertEquals(1, mongo.sent.size)
    }

    @Test
    fun `the version handed back is the one before the change unless AFTER is asked for`() =
        runTest {
            val mongo = FakeMongo { okReply("value" to Document("_id", 1)) }

            mongo.orders.findOneAndUpdate(
                Document("_id", 1),
                Document("\$inc", Document("total", 1)),
                returning = ReturnDocument.AFTER,
            )

            assertEquals(true, mongo.lastCommand["new"])
        }

    @Test
    fun `a sort decides which document is claimed, and a projection cuts what comes back`() =
        runTest {
            val mongo = FakeMongo { okReply("value" to Document("_id", 1)) }

            mongo.orders.findOneAndUpdate(
                filter = Document("sent", false),
                update = Document("\$set", Document("sent", true)),
                sort = Document("placed", 1),
                projection = Document("_id", 1),
                upsert = true,
            )

            assertEquals(Document("placed", 1), mongo.lastCommand["sort"])
            // MongoDB spells the projection `fields` on this command and nowhere else.
            assertEquals(Document("_id", 1), mongo.lastCommand["fields"])
            assertEquals(true, mongo.lastCommand["upsert"])
        }

    @Test
    fun `nothing matching is null rather than a failure`() = runTest {
        val mongo = FakeMongo { okReply("value" to null) }

        assertNull(mongo.orders.findOneAndUpdate(Document("_id", 9), Document("\$set", Document("a", 1))))
    }

    @Test
    fun `a delete in place removes the document it hands back`() = runTest {
        val mongo = FakeMongo { okReply("value" to Document("_id", 1)) }

        val removed = mongo.orders.findOneAndDelete(Document("sent", true), sort = Document("_id", 1))

        assertEquals(Document("_id", 1), removed)
        assertEquals(
            Document("findAndModify", "orders")
                .append("query", Document("sent", true))
                .append("sort", Document("_id", 1))
                .append("remove", true),
            mongo.lastCommand,
        )
    }

    @Test
    fun `a value that is not a document is a failure rather than a silent null`() = runTest {
        val mongo = FakeMongo { okReply("value" to "not a document") }

        assertFailsWith<EmbeddedMongoException> {
            mongo.orders.findOneAndDelete(Document("_id", 1))
        }
    }
}

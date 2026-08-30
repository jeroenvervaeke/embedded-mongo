package io.github.jeroenvervaeke.embeddedmongodb

import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFailsWith
import kotlinx.coroutines.test.runTest
import org.bson.Document

class MongoDatabaseTest {
    @Test
    fun `a collection knows the database it belongs to without being told again`() = runTest {
        val mongo = FakeMongo { singleBatch(emptyList()) }

        mongo.orders.countDocuments()

        assertEquals("shop.orders", mongo.orders.namespace)
        assertEquals(listOf("shop"), mongo.commands.databases)
    }

    @Test
    fun `counting reads the documents rather than the metadata`() = runTest {
        val mongo = FakeMongo { singleBatch(listOf(Document("count", 5180))) }

        assertEquals(5180L, mongo.orders.countDocuments())
        assertEquals(
            listOf(Document("\$count", "count")),
            mongo.lastCommand.pipeline(),
        )
    }

    @Test
    fun `a filtered count matches before it counts`() = runTest {
        val mongo = FakeMongo { singleBatch(listOf(Document("count", 2))) }

        assertEquals(2L, mongo.orders.countDocuments(Document("paid", true)))
        assertEquals(
            listOf(Document("\$match", Document("paid", true)), Document("\$count", "count")),
            mongo.lastCommand.pipeline(),
        )
    }

    @Test
    fun `a count over an empty collection is zero, which is the row MongoDB does not emit`() =
        runTest {
            val mongo = FakeMongo { singleBatch(emptyList()) }

            assertEquals(0L, mongo.orders.countDocuments())
        }

    @Test
    fun `the estimated count is the cheap one the engine answers from metadata`() = runTest {
        val mongo = FakeMongo { okReply("n" to 5180) }

        assertEquals(5180L, mongo.orders.estimatedDocumentCount())
        assertEquals(Document("count", "orders"), mongo.lastCommand)
    }

    @Test
    fun `a count reply without a number is a failure rather than a silent zero`() = runTest {
        val mongo = FakeMongo { okReply() }

        assertFailsWith<EmbeddedMongoException> { mongo.orders.estimatedDocumentCount() }
    }

    @Test
    fun `collection names are read through the cursor listCollections opens`() = runTest {
        val mongo = FakeMongo {
            singleBatch(listOf(Document("name", "orders"), Document("name", "customers")))
        }

        assertEquals(listOf("orders", "customers"), mongo.database.listCollectionNames())
        assertEquals(
            Document("listCollections", 1).append("nameOnly", true),
            mongo.sent.first(),
        )
    }

    @Test
    fun `creating a collection carries the options it was given`() = runTest {
        val mongo = FakeMongo { okReply() }

        mongo.database.createCollection("audit", Document("capped", true).append("size", 4096))

        assertEquals(
            Document("create", "audit").append("capped", true).append("size", 4096),
            mongo.lastCommand,
        )
    }

    @Test
    fun `creating a collection that already exists is the state the caller asked for`() = runTest {
        val mongo = FakeMongo { mongoError(MongoErrorCode.NAMESPACE_EXISTS, "collection exists") }

        mongo.database.createCollection("orders")
    }

    @Test
    fun `dropping a database that is not there is a no-op`() = runTest {
        val mongo = FakeMongo { mongoError(MongoErrorCode.NAMESPACE_NOT_FOUND, "ns not found") }

        mongo.database.drop()

        assertEquals(Document("dropDatabase", 1), mongo.lastCommand)
    }

    @Test
    fun `a command with no builder above it still reaches the engine`() = runTest {
        val mongo = FakeMongo { okReply("version" to "9.0.0") }

        val reply = mongo.database.runCommand(Document("buildInfo", 1))

        assertEquals("9.0.0", reply["version"])
    }
}

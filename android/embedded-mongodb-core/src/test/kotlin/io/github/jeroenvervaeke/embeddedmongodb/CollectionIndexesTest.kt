package io.github.jeroenvervaeke.embeddedmongodb

import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFailsWith
import kotlin.test.assertTrue
import kotlinx.coroutines.test.runTest
import org.bson.Document

class CollectionIndexesTest {
    @Test
    fun `an index with no name is given MongoDB's own`() = runTest {
        val mongo = FakeMongo { okReply() }

        val name = mongo.orders.createIndex(Indexes.ascending("customer"))

        assertEquals("customer_1", name)
        assertEquals(
            Document("key", Document("customer", 1)).append("name", "customer_1"),
            mongo.lastCommand.indexes().single(),
        )
    }

    @Test
    fun `a compound index is named after every key in it, in order`() = runTest {
        val mongo = FakeMongo { okReply() }

        val name = mongo.orders.createIndex(
            Indexes.compound(Indexes.ascending("customer"), Indexes.descending("placed")),
        )

        assertEquals("customer_1_placed_-1", name)
    }

    @Test
    fun `a geospatial index carries the keyword rather than a direction`() = runTest {
        val mongo = FakeMongo { okReply() }

        mongo.orders.createIndex(Indexes.geo2dsphere("loc"))

        assertEquals(Document("loc", "2dsphere"), mongo.lastCommand.indexes().single()["key"])
    }

    @Test
    fun `every option the caller named reaches the specification under MongoDB's spelling`() =
        runTest {
            val mongo = FakeMongo { okReply() }

            mongo.orders.createIndex(
                Indexes.text("name", "brand"),
                IndexOptions(
                    name = "search",
                    unique = true,
                    sparse = true,
                    partialFilter = Document("paid", true),
                    expireAfterSeconds = 60,
                    weights = Document("name", 10),
                    defaultLanguage = "english",
                ),
            )

            assertEquals(
                Document("key", Document("name", "text").append("brand", "text"))
                    .append("name", "search")
                    .append("unique", true)
                    .append("sparse", true)
                    .append("partialFilterExpression", Document("paid", true))
                    .append("expireAfterSeconds", 60L)
                    .append("weights", Document("name", 10))
                    .append("default_language", "english"),
                mongo.lastCommand.indexes().single(),
            )
        }

    @Test
    fun `an option the caller left alone is left out rather than sent as a default`() = runTest {
        val mongo = FakeMongo { okReply() }

        mongo.orders.createIndex(Indexes.ascending("customer"))

        assertEquals(listOf("key", "name"), mongo.lastCommand.indexes().single().keys.toList())
    }

    @Test
    fun `several indexes are built by one command`() = runTest {
        val mongo = FakeMongo { okReply() }

        val names = mongo.orders.createIndexes(
            listOf(
                IndexModel(Indexes.geo2dsphere("loc")),
                IndexModel(Indexes.ascending("customer"), IndexOptions(name = "by_customer")),
            ),
        )

        assertEquals(listOf("loc_2dsphere", "by_customer"), names)
        assertEquals(1, mongo.sent.size)
    }

    @Test
    fun `building no indexes is refused here rather than by the engine`() = runTest {
        assertFailsWith<IllegalArgumentException> { FakeMongo().orders.createIndexes(emptyList()) }
    }

    @Test
    fun `an index over no fields indexes nothing and is refused`() {
        assertFailsWith<IllegalArgumentException> { Indexes.ascending() }
    }

    @Test
    fun `the indexes are read back through the cursor listIndexes opens`() = runTest {
        val mongo = FakeMongo { singleBatch(listOf(Document("name", "_id_"))) }

        assertEquals(listOf(Document("name", "_id_")), mongo.orders.listIndexes())
        assertEquals(Document("listIndexes", "orders"), mongo.sent.first())
    }

    @Test
    fun `dropping an index that is not there is what the caller asked for, not a failure`() =
        runTest {
            val mongo = FakeMongo { mongoError(MongoErrorCode.INDEX_NOT_FOUND, "index not found") }

            mongo.orders.dropIndex("by_customer")

            assertEquals(
                Document("dropIndexes", "orders").append("index", "by_customer"),
                mongo.lastCommand,
            )
        }

    @Test
    fun `an index MongoDB refused for any other reason is still a failure`() = runTest {
        val mongo = FakeMongo { mongoError(72, "cannot drop _id index") }

        val failure = assertFailsWith<EmbeddedMongoException> { mongo.orders.dropIndex("_id_") }

        assertEquals(72, failure.code)
    }

    @Test
    fun `dropping a collection that is not there is a no-op`() = runTest {
        val mongo = FakeMongo { mongoError(MongoErrorCode.NAMESPACE_NOT_FOUND, "ns not found") }

        mongo.orders.drop()

        assertEquals(Document("drop", "orders"), mongo.lastCommand)
    }

    @Test
    fun `a collection that refused to drop for another reason still fails`() = runTest {
        val mongo = FakeMongo { mongoError(BridgeError.ENGINE_ERROR.code, "the engine is closing") }

        val failure = assertFailsWith<EmbeddedMongoException> { mongo.orders.drop() }

        assertTrue(failure.bridgeError == BridgeError.ENGINE_ERROR, "${failure.code}")
    }
}

@Suppress("UNCHECKED_CAST")
private fun Document.indexes(): List<Document> = this["indexes"] as List<Document>

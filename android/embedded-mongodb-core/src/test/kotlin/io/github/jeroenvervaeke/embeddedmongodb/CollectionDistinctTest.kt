package io.github.jeroenvervaeke.embeddedmongodb

import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFailsWith
import kotlinx.coroutines.test.runTest
import org.bson.Document

class CollectionDistinctTest {
    @Test
    fun `the engine does the de-duplication and only the values cross`() = runTest {
        val mongo = FakeMongo { okReply("values" to listOf("cafe", "coffee_shop")) }

        val categories = mongo.orders.distinct("cat")

        assertEquals(listOf("cafe", "coffee_shop"), categories)
        assertEquals(Document("distinct", "orders").append("key", "cat"), mongo.lastCommand)
    }

    @Test
    fun `a filter narrows what is looked at, and is left out when there is none`() = runTest {
        val mongo = FakeMongo { okReply("values" to listOf("cafe")) }

        mongo.orders.distinct("cat", Document("confidence", Document("\$gte", 0.8)))

        assertEquals(Document("confidence", Document("\$gte", 0.8)), mongo.lastCommand["query"])
    }

    @Test
    fun `a field nothing stores is an empty list rather than a failure`() = runTest {
        val mongo = FakeMongo { okReply("values" to emptyList<Any>()) }

        assertEquals(emptyList(), mongo.orders.distinct("nonesuch"))
    }

    @Test
    fun `a reply carrying no values is a failure rather than an empty list`() = runTest {
        val mongo = FakeMongo { okReply() }

        assertFailsWith<EmbeddedMongoException> { mongo.orders.distinct("cat") }
    }
}

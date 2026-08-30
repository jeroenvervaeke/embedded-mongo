package io.github.jeroenvervaeke.embeddedmongodb

import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFailsWith
import kotlin.test.assertSame
import org.bson.BsonArray
import org.bson.BsonBoolean
import org.bson.BsonDocument
import org.bson.BsonInt64
import org.bson.BsonString
import org.bson.Document

class BsonsTest {
    @Test
    fun `a document the caller built is the document that is sent, not a copy of it`() {
        // Which is what lets an application show the query it ran and mean the same object.
        val filter = Document("paid", true)

        assertSame(filter, filter.toDocument())
    }

    @Test
    fun `a Bson built by something else is converted, nested values included`() {
        val filter = BsonDocument("customer", BsonString("ada"))
            .append("totals", BsonArray(listOf(BsonInt64(1), BsonInt64(2))))
            .append("nested", BsonDocument("paid", BsonBoolean.TRUE))

        val converted: Document = filter.toDocument()

        assertEquals(
            Document("customer", "ada")
                .append("totals", listOf(1L, 2L))
                .append("nested", Document("paid", true)),
            converted,
        )
    }

    @Test
    fun `a number the engine wrote at any width is read as the number it is`() {
        assertEquals(7L, Document("n", 7).requiredLong("n"))
        assertEquals(7L, Document("n", 7L).requiredLong("n"))
        assertEquals(7L, Document("n", 7.0).requiredLong("n"))
    }

    @Test
    fun `a field that is missing or is not a number is a failure naming what was there`() {
        val failure = assertFailsWith<EmbeddedMongoException> {
            Document("ok", 1.0).requiredLong("n")
        }

        assertEquals(NO_ERROR_CODE, failure.code)
        assertEquals("the reply carries no numeric `n` (fields: ok)", failure.message)
    }
}

package io.github.jeroenvervaeke.embeddedmongodb

import java.nio.ByteBuffer
import java.nio.ByteOrder
import java.util.Date
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFailsWith
import kotlin.test.assertTrue
import org.bson.BSONException
import org.bson.Document
import org.bson.types.Binary
import org.bson.types.Decimal128
import org.bson.types.ObjectId

class BsonCodecTest {
    @Test
    fun `round trip preserves every value a reply can hold`() {
        val document = Document("_id", ObjectId())
            .append("text", "café")
            .append("int", 42)
            .append("long", Long.MAX_VALUE)
            .append("double", 1.5)
            .append("decimal", Decimal128.parse("1.25"))
            .append("flag", true)
            .append("missing", null)
            .append("when", Date(1_700_000_000_000))
            .append("binary", Binary(byteArrayOf(1, 2, 3)))
            .append("nested", Document("inner", listOf(1, 2, 3)))
            .append("documents", listOf(Document("a", 1), Document("b", 2)))

        assertEquals(document, BsonCodec.decode(BsonCodec.encode(document)))
    }

    @Test
    fun `round trip preserves an empty document`() {
        assertEquals(Document(), BsonCodec.decode(BsonCodec.encode(Document())))
    }

    @Test
    fun `bytes that are not a document are reported as an engine failure`() {
        val failure = assertFailsWith<EmbeddedMongoException> {
            BsonCodec.decode(byteArrayOf(1, 2, 3, 4, 5, 6, 7, 8))
        }

        assertEquals(NO_ERROR_CODE, failure.code)
        assertTrue(failure.cause is BSONException)
    }

    @Test
    fun `encoding writes the whole document, not a truncated buffer`() {
        val encoded = BsonCodec.encode(Document("text", "x".repeat(1000)))

        val declaredLength = ByteBuffer.wrap(encoded).order(ByteOrder.LITTLE_ENDIAN).int
        assertEquals(encoded.size, declaredLength)
    }
}

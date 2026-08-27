package io.github.jeroenvervaeke.embeddedmongodb

import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFailsWith
import kotlin.test.assertSame
import org.bson.Document

class CommandReplyTest {
    @Test
    fun `a successful reply is returned unchanged`() {
        val reply = okReply("n" to 1)

        assertSame(reply, checkedReply(reply))
    }

    @Test
    fun `ok is accepted whichever numeric type the engine used`() {
        val reply = Document("ok", 1)

        assertSame(reply, checkedReply(reply))
    }

    @Test
    fun `a failed command becomes an exception carrying its message and code`() {
        val reply = Document("ok", 0.0)
            .append("errmsg", "no such collection")
            .append("code", 26)

        val failure = assertFailsWith<EmbeddedMongoException> { checkedReply(reply) }

        assertEquals("no such collection", failure.message)
        assertEquals(26, failure.code)
    }

    @Test
    fun `a failed command without details still fails`() {
        val failure = assertFailsWith<EmbeddedMongoException> { checkedReply(Document("ok", 0.0)) }

        assertEquals(NO_ERROR_CODE, failure.code)
    }

    @Test
    fun `a write error fails the command even though the command itself reported ok`() {
        val reply = okReply(
            "writeErrors" to listOf(Document("code", 11000).append("errmsg", "duplicate key")),
        )

        val failure = assertFailsWith<EmbeddedMongoException> { checkedReply(reply) }

        assertEquals("duplicate key", failure.message)
        assertEquals(11000, failure.code)
    }

    @Test
    fun `an empty write error array is what a successful write reports`() {
        val reply = okReply("writeErrors" to emptyList<Document>())

        assertSame(reply, checkedReply(reply))
    }

    @Test
    fun `a write concern error fails the command`() {
        val reply = okReply(
            "writeConcernError" to Document("code", 64).append("errmsg", "waiting for replication"),
        )

        val failure = assertFailsWith<EmbeddedMongoException> { checkedReply(reply) }

        assertEquals(64, failure.code)
    }

    @Test
    fun `a reply without an ok field is not treated as success`() {
        val failure = assertFailsWith<EmbeddedMongoException> { checkedReply(Document("n", 1)) }

        assertEquals(NO_ERROR_CODE, failure.code)
    }
}

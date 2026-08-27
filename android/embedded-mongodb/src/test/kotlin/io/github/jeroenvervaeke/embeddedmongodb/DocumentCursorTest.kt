package io.github.jeroenvervaeke.embeddedmongodb

import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFailsWith
import kotlin.test.assertTrue
import org.bson.Document

class DocumentCursorTest {
    @Test
    fun `a cursor the engine already exhausted is read without asking for more`() {
        val runner = FakeRunner(emptyList())

        val read = DocumentCursor(runner, DATABASE, cursorReply(0, "firstBatch", documents(1..3))).toList()

        assertEquals(documents(1..3), read)
        assertTrue(runner.commands.isEmpty())
    }

    @Test
    fun `batches are fetched until the engine reports the cursor as exhausted`() {
        val runner = FakeRunner(
            listOf(
                cursorReply(7, "nextBatch", documents(3..4)),
                cursorReply(0, "nextBatch", documents(5..5)),
            ),
        )

        val read = DocumentCursor(runner, DATABASE, cursorReply(7, "firstBatch", documents(1..2))).toList()

        assertEquals(documents(1..5), read)
        assertEquals(
            listOf(Document("getMore", 7L).append("collection", "orders")),
            runner.commands.distinct(),
        )
    }

    @Test
    fun `the collection is taken from the namespace, dots in its name included`() {
        val runner = FakeRunner(listOf(cursorReply(0, "nextBatch", emptyList())))
        val reply = cursorReply(7, "firstBatch", emptyList(), namespace = "shop.orders.2026")

        DocumentCursor(runner, DATABASE, reply).toList()

        assertEquals("orders.2026", runner.commands.single().getString("collection"))
    }

    @Test
    fun `abandoning a cursor tells the engine to drop it`() {
        val runner = FakeRunner(listOf(okReply()))
        val cursor = DocumentCursor(runner, DATABASE, cursorReply(7, "firstBatch", documents(1..2)))

        cursor.use { it.first() }

        assertEquals(
            Document("killCursors", "orders").append("cursors", listOf(7L)),
            runner.commands.single(),
        )
    }

    @Test
    fun `closing an exhausted cursor asks the engine for nothing`() {
        val runner = FakeRunner(emptyList())
        val cursor = DocumentCursor(runner, DATABASE, cursorReply(0, "firstBatch", documents(1..2)))

        cursor.use { it.toList() }

        assertTrue(runner.commands.isEmpty())
    }

    @Test
    fun `closing twice kills the cursor once`() {
        val runner = FakeRunner(listOf(okReply()))
        val cursor = DocumentCursor(runner, DATABASE, cursorReply(7, "firstBatch", emptyList()))

        cursor.close()
        cursor.close()

        assertEquals(1, runner.commands.size)
    }

    @Test
    fun `iterating a second time fails rather than silently returning nothing`() {
        val cursor = DocumentCursor(FakeRunner(emptyList()), DATABASE, cursorReply(0, "firstBatch", documents(1..1)))
        cursor.toList()

        assertFailsWith<IllegalStateException> { cursor.toList() }
    }

    @Test
    fun `a reply that carries no cursor is reported`() {
        assertFailsWith<EmbeddedMongoException> {
            DocumentCursor(FakeRunner(emptyList()), DATABASE, okReply("n" to 1))
        }
    }

    @Test
    fun `a cursor reply without a namespace is reported`() {
        val reply = okReply("cursor" to Document("id", 0L).append("firstBatch", emptyList<Document>()))

        assertFailsWith<EmbeddedMongoException> { DocumentCursor(FakeRunner(emptyList()), DATABASE, reply) }
    }

    @Test
    fun `a batch holding something that is not a document is reported`() {
        val reply = cursorReply(0, "firstBatch", emptyList()).also {
            it.getEmbedded(listOf("cursor"), Document::class.java)["firstBatch"] = listOf("not a document")
        }

        assertFailsWith<EmbeddedMongoException> { DocumentCursor(FakeRunner(emptyList()), DATABASE, reply) }
    }

    @Test
    fun `a failure while fetching a batch reaches the caller`() {
        val runner = CommandRunner { _, _ -> throw EmbeddedMongoException("cursor not found", 43) }
        val cursor = DocumentCursor(runner, DATABASE, cursorReply(7, "firstBatch", documents(1..1)))

        val failure = assertFailsWith<EmbeddedMongoException> { cursor.toList() }

        assertEquals(43, failure.code)
    }
}

private const val DATABASE = "shop"

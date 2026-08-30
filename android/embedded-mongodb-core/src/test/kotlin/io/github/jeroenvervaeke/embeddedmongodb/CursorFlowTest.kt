package io.github.jeroenvervaeke.embeddedmongodb

import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFailsWith
import kotlin.test.assertTrue
import kotlinx.coroutines.flow.take
import kotlinx.coroutines.flow.toList
import kotlinx.coroutines.test.runTest
import org.bson.Document

class CursorFlowTest {
    @Test
    fun `documents arrive across as many batches as the engine needs`() = runTest {
        val mongo = FakeMongo { command ->
            when {
                command.containsKey("getMore") -> cursorReply(0, "nextBatch", documents(3..4))
                else -> cursorReply(7, "firstBatch", documents(1..2))
            }
        }

        val read = mongo.database.runCursorCommand(Document("find", "orders")).toList()

        assertEquals(documents(1..4), read)
    }

    @Test
    fun `a getMore names the cursor and the collection the reply reported`() = runTest {
        val mongo = FakeMongo { command ->
            if (command.containsKey("getMore")) cursorReply(0, "nextBatch", emptyList())
            else cursorReply(7, "firstBatch", documents(1..1), namespace = "shop.line.items")
        }

        mongo.database.runCursorCommand(Document("find", "line.items")).toList()

        // The namespace names the database as well, and a collection name may hold further dots:
        // only the first separator is the prefix.
        assertEquals(
            Document("getMore", 7L).append("collection", "line.items"),
            mongo.lastCommand,
        )
    }

    @Test
    fun `a collector that stops early leaves no cursor behind`() = runTest {
        val mongo = FakeMongo { command ->
            when {
                command.containsKey("killCursors") -> okReply()
                command.containsKey("getMore") -> error("no second batch was due")
                else -> cursorReply(7, "firstBatch", documents(1..500))
            }
        }

        val read = mongo.database.runCursorCommand(Document("find", "orders")).take(1).toList()

        assertEquals(documents(1..1), read)
        assertEquals(
            Document("killCursors", "orders").append("cursors", listOf(7L)),
            mongo.lastCommand,
        )
    }

    @Test
    fun `a collector that throws leaves no cursor behind, and its own failure is what surfaces`() =
        runTest {
            val mongo = FakeMongo { command ->
                if (command.containsKey("killCursors")) okReply()
                else cursorReply(7, "firstBatch", documents(1..5))
            }

            val failure = assertFailsWith<IllegalStateException> {
                mongo.database.runCursorCommand(Document("find", "orders")).collect {
                    error("the collector gave up")
                }
            }

            assertEquals("the collector gave up", failure.message)
            assertTrue(mongo.sent.any { it.containsKey("killCursors") }, "${mongo.sent}")
        }

    @Test
    fun `a cursor that ran out is not killed, because the engine already dropped it`() = runTest {
        val mongo = FakeMongo { singleBatch(documents(1..2)) }

        mongo.database.runCursorCommand(Document("find", "orders")).toList()

        assertEquals(1, mongo.sent.size, "${mongo.sent}")
    }

    @Test
    fun `a killCursors that fails is attached to the failure that caused it`() = runTest {
        val mongo = FakeMongo { command ->
            if (command.containsKey("killCursors")) mongoError(59, "no such command")
            else cursorReply(7, "firstBatch", documents(1..5))
        }

        val failure = assertFailsWith<IllegalStateException> {
            mongo.database.runCursorCommand(Document("find", "orders")).collect {
                error("the collector gave up")
            }
        }

        assertEquals(
            listOf("no such command"),
            failure.suppressedExceptions.map { it.message },
        )
    }

    @Test
    fun `a reply that carries no cursor is a failure rather than an empty result`() = runTest {
        val mongo = FakeMongo { okReply("n" to 1) }

        val failure = assertFailsWith<EmbeddedMongoException> {
            mongo.database.runCursorCommand(Document("find", "orders")).toList()
        }

        assertTrue(failure.message!!.contains("no cursor"), failure.message!!)
    }

    @Test
    fun `a cursor reply that names no id is a failure`() = runTest {
        val mongo = FakeMongo {
            okReply("cursor" to Document("ns", "shop.orders").append("firstBatch", emptyList<Document>()))
        }

        assertFailsWith<EmbeddedMongoException> {
            mongo.database.runCursorCommand(Document("find", "orders")).toList()
        }
    }

    @Test
    fun `a batch holding something that is not a document is a failure`() = runTest {
        val mongo = FakeMongo {
            cursorReply(0, "firstBatch", emptyList()).also { reply ->
                (reply["cursor"] as Document)["firstBatch"] = listOf("not a document")
            }
        }

        assertFailsWith<EmbeddedMongoException> {
            mongo.database.runCursorCommand(Document("find", "orders")).toList()
        }
    }

    @Test
    fun `a namespace naming no collection is a failure, because no getMore could name one`() =
        runTest {
            val mongo = FakeMongo { cursorReply(7, "firstBatch", documents(1..1), namespace = "shop") }

            assertFailsWith<EmbeddedMongoException> {
                mongo.database.runCursorCommand(Document("find", "orders")).toList()
            }
        }

    @Test
    fun `an abandoned cursor is killed once, not once per way out of the flow`() = runTest {
        val mongo = FakeMongo { command ->
            if (command.containsKey("killCursors")) okReply()
            else cursorReply(7, "firstBatch", documents(1..5))
        }

        assertFailsWith<IllegalStateException> {
            mongo.database.runCursorCommand(Document("find", "orders")).collect {
                error("the collector gave up")
            }
        }

        assertEquals(1, mongo.sent.count { it.containsKey("killCursors") }, "${mongo.sent}")
    }

    @Test
    fun `a failure fetching the next batch reaches the collector`() = runTest {
        val mongo = FakeMongo { command ->
            when {
                command.containsKey("getMore") -> mongoError(59, "the engine gave up")
                command.containsKey("killCursors") -> okReply()
                else -> cursorReply(7, "firstBatch", documents(1..2))
            }
        }

        val failure = assertFailsWith<EmbeddedMongoException> {
            mongo.database.runCursorCommand(Document("find", "orders")).toList()
        }

        assertEquals("the engine gave up", failure.message)
    }

    @Test
    fun `a cursor reply without a namespace is reported`() = runTest {
        val mongo = FakeMongo {
            okReply(
                "cursor" to Document("id", 0L).append("firstBatch", emptyList<Document>()),
            )
        }

        assertFailsWith<EmbeddedMongoException> {
            mongo.database.runCursorCommand(Document("find", "orders")).toList()
        }
    }

    @Test
    fun `collecting the flow twice asks the engine twice, because a flow is cold`() = runTest {
        val mongo = FakeMongo { singleBatch(documents(1..2)) }
        val found = mongo.database.runCursorCommand(Document("find", "orders"))

        assertEquals(documents(1..2), found.toList())
        assertEquals(documents(1..2), found.toList())

        assertEquals(2, mongo.sent.size)
    }
}

package io.github.jeroenvervaeke.embeddedmongodb

import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFailsWith
import kotlin.test.assertTrue
import kotlinx.coroutines.flow.take
import kotlinx.coroutines.flow.toList
import kotlinx.coroutines.runBlocking
import org.bson.Document

class EmbeddedMongoTest {
    @Test
    fun `a command crosses the bridge as BSON and comes back as a document`() {
        val engine = FakeEngine { okReply("n" to 1) }
        val mongo = EmbeddedMongo(engine, guard(onMainThread = false))

        val reply = mongo.runCommandBlocking("shop", Document("count", "orders"))

        assertEquals(1, reply.getInteger("n"))
        assertEquals(listOf(Document("count", "orders")), engine.commands)
        assertEquals(listOf("shop"), engine.databases)
    }

    @Test
    fun `a write reaches the engine journalled`() {
        val engine = FakeEngine { okReply("n" to 1) }
        val mongo = EmbeddedMongo(engine, guard(onMainThread = false))

        mongo.runCommandBlocking("shop", Document("insert", "orders"))

        assertEquals(
            Document("w", 1).append("j", true),
            engine.commands.single()["writeConcern"],
        )
    }

    @Test
    fun `a command the engine rejected is raised, not returned`() {
        val engine = FakeEngine { Document("ok", 0.0).append("errmsg", "unknown").append("code", 59) }
        val mongo = EmbeddedMongo(engine, guard(onMainThread = false))

        val failure = assertFailsWith<EmbeddedMongoException> {
            mongo.runCommandBlocking("shop", Document("nonesuch", 1))
        }

        assertEquals(59, failure.code)
    }

    @Test
    fun `a blocking command on the main thread fails before reaching the engine`() {
        val engine = FakeEngine { okReply() }
        val mongo = EmbeddedMongo(engine, guard(onMainThread = true))

        assertFailsWith<IllegalStateException> { mongo.runCommandBlocking("shop", Document("ping", 1)) }

        assertTrue(engine.commands.isEmpty())
    }

    @Test
    fun `a suspending command called from the main thread runs on the database thread`() {
        val caller = Thread.currentThread()
        val engine = FakeEngine { okReply() }
        val mongo = EmbeddedMongo(engine, MainThreadGuard({ Thread.currentThread() == caller }))

        runBlocking { mongo.runCommand("shop", Document("ping", 1)) }

        // The coroutine debugger appends its own suffix to the thread name.
        assertTrue(engine.threads.single().startsWith("embedded-mongodb"))
    }

    @Test
    fun `every suspending command shares the one database thread`() {
        val engine = FakeEngine { okReply() }
        val mongo = EmbeddedMongo(engine, guard(onMainThread = false))

        runBlocking { repeat(4) { mongo.runCommand("shop", Document("ping", 1)) } }

        assertEquals(1, engine.threads.distinct().size)
    }

    @Test
    fun `documents are emitted across as many batches as the engine needs`() {
        val engine = FakeEngine { command ->
            if (command.containsKey("getMore")) {
                cursorReply(0, "nextBatch", documents(3..4))
            } else {
                cursorReply(7, "firstBatch", documents(1..2))
            }
        }
        val mongo = EmbeddedMongo(engine, guard(onMainThread = false))

        val read = runBlocking { mongo.getDatabase("shop").runCursorCommand(Document("find", "orders")).toList() }

        assertEquals(documents(1..4), read)
    }

    @Test
    fun `a collector that stops early leaves no cursor behind`() {
        val killed = CountDownLatch(1)
        // A batch far larger than the buffer that flowOn puts between the two coroutines, so the
        // cursor is guaranteed to still hold documents when the collector stops.
        val engine = FakeEngine { command ->
            when {
                command.containsKey("killCursors") -> okReply().also { killed.countDown() }
                command.containsKey("getMore") ->
                    error("the collector stopped after one document; no second batch was due")
                else -> cursorReply(7, "firstBatch", documents(1..500))
            }
        }
        val mongo = EmbeddedMongo(engine, guard(onMainThread = false))

        val read = runBlocking { mongo.getDatabase("shop").runCursorCommand(Document("find", "orders")).take(1).toList() }

        assertEquals(documents(1..1), read)
        assertTrue(killed.await(5, TimeUnit.SECONDS), "the abandoned cursor was never killed")
    }

    @Test
    fun `closing releases the engine and refuses later commands`() {
        val engine = FakeEngine { okReply() }
        val mongo = EmbeddedMongo(engine, guard(onMainThread = false))

        mongo.close()

        assertEquals(1, engine.closes)
        assertFailsWith<IllegalStateException> { mongo.runCommandBlocking("shop", Document("ping", 1)) }
    }

    @Test
    fun `a suspending command after close fails rather than hanging`() {
        val engine = FakeEngine { okReply() }
        val mongo = EmbeddedMongo(engine, guard(onMainThread = false))
        mongo.close()

        assertFailsWith<IllegalStateException> {
            runBlocking { mongo.runCommand("shop", Document("ping", 1)) }
        }
    }

    @Test
    fun `collecting documents after close fails rather than hanging`() {
        val engine = FakeEngine { okReply() }
        val mongo = EmbeddedMongo(engine, guard(onMainThread = false))
        mongo.close()

        assertFailsWith<IllegalStateException> {
            runBlocking { mongo.getDatabase("shop").runCursorCommand(Document("find", "orders")).toList() }
        }
    }

    @Test
    fun `closing twice releases the engine once`() {
        val engine = FakeEngine { okReply() }
        val mongo = EmbeddedMongo(engine, guard(onMainThread = false))

        mongo.close()
        mongo.close()

        assertEquals(1, engine.closes)
    }

    @Test
    fun `closing on the main thread warns rather than throwing`() {
        val reported = mutableListOf<String>()
        val engine = FakeEngine { okReply() }
        val mongo = EmbeddedMongo(engine, guard(onMainThread = true, report = reported::add))

        mongo.close()

        assertEquals(1, engine.closes)
        assertEquals(1, reported.size)
    }
}

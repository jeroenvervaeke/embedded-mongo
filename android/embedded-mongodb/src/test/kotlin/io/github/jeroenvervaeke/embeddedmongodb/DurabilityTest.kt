package io.github.jeroenvervaeke.embeddedmongodb

import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertNull
import kotlin.test.assertSame
import org.bson.Document

class DurabilityTest {
    @Test
    fun `a write is journalled before it is acknowledged`() {
        val insert = Document("insert", "orders").append("documents", listOf(Document("n", 1)))

        val concern = durable(insert).get("writeConcern")

        assertEquals(Document("w", 1).append("j", true), concern)
    }

    @Test
    fun `a caller who named a write concern keeps it`() {
        val insert = Document("insert", "orders").append("writeConcern", Document("w", 0))

        assertSame(insert, durable(insert))
    }

    @Test
    fun `a read is left alone, because reads reject a write concern`() {
        val find = Document("find", "orders")

        assertSame(find, durable(find))
    }

    @Test
    fun `the caller's document is not edited`() {
        val insert = Document("insert", "orders")

        durable(insert)

        assertNull(insert["writeConcern"])
    }

    @Test
    fun `a command with no name at all passes through`() {
        val empty = Document()

        assertSame(empty, durable(empty))
    }
}

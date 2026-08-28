package io.github.jeroenvervaeke.embeddedmongodb

import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFailsWith
import kotlin.test.assertNull
import kotlin.test.assertTrue
import kotlinx.coroutines.runBlocking
import org.bson.Document

class FreeDiskFloorTest {
    @Test
    fun `a floor of zero is refused because it is not a floor`() {
        assertFailsWith<IllegalArgumentException> { FreeDiskFloor.ofMebibytes(0) }
    }

    @Test
    fun `the smallest floor is accepted`() {
        assertEquals(1, FreeDiskFloor.ofMebibytes(1).mebibytes)
    }

    @Test
    fun `the engine's own default is the 500 MB MongoDB ships`() {
        assertEquals(500, FreeDiskFloor.ENGINE_DEFAULT.mebibytes)
    }

    @Test
    fun `the index build floor is set in mebibytes`() {
        val (indexBuild, _) = freeDiskFloorCommands(FreeDiskFloor.ofMebibytes(32))

        assertEquals(32L, indexBuild["indexBuildMinAvailableDiskSpaceMB"])
    }

    @Test
    fun `the spilling floor is set in bytes`() {
        val (_, spilling) = freeDiskFloorCommands(FreeDiskFloor.ofMebibytes(32))

        assertEquals(33_554_432L, spilling["internalQuerySpillingMinAvailableDiskSpaceBytes"])
    }

    /**
     * `setParameter` reports the previous value in a field named `was`, so one command carrying
     * both knobs would answer with two fields of that name.
     */
    @Test
    fun `the two knobs are set by two commands`() {
        val commands = freeDiskFloorCommands(FreeDiskFloor.ofMebibytes(32))

        assertEquals(2, commands.size)
        assertTrue(commands.all { it.keys.first() == "setParameter" })
    }

    @Test
    fun `a large floor does not overflow the byte count`() {
        val (_, spilling) = freeDiskFloorCommands(FreeDiskFloor.ofMebibytes(Int.MAX_VALUE))

        assertEquals(
            Int.MAX_VALUE.toLong() * 1024 * 1024,
            spilling["internalQuerySpillingMinAvailableDiskSpaceBytes"],
        )
    }

    @Test
    fun `the floors the engine reports are read in the unit each knob uses`() {
        val engine = FakeEngine {
            okReply(
                "indexBuildMinAvailableDiskSpaceMB" to 500L,
                "internalQuerySpillingMinAvailableDiskSpaceBytes" to 524_288_000L,
            )
        }

        val floors = EmbeddedMongo(engine, guard(onMainThread = false)).freeDiskFloorsBlocking()

        assertEquals(ReportedFloors(500L, 524_288_000L), floors)
    }

    /**
     * A knob MongoDB renamed must be an error rather than a floor this library never set: an
     * application would otherwise size an index build against a number that is not in force.
     */
    @Test
    fun `a reply missing one of the knobs is raised, not defaulted`() {
        val engine = FakeEngine { okReply("indexBuildMinAvailableDiskSpaceMB" to 500L) }
        val database = EmbeddedMongo(engine, guard(onMainThread = false))

        val failure = assertFailsWith<EmbeddedMongoException> { database.freeDiskFloorsBlocking() }

        assertTrue(
            failure.message.orEmpty().contains("internalQuerySpillingMinAvailableDiskSpaceBytes"),
            failure.message.orEmpty(),
        )
    }

    @Test
    fun `setting the floor sends both knobs to the admin database`() {
        val engine = FakeEngine { okReply() }
        val database = EmbeddedMongo(engine, guard(onMainThread = false))

        database.setFreeDiskFloorBlocking(FreeDiskFloor.ofMebibytes(64))

        assertEquals(listOf("admin", "admin"), engine.databases)
        assertEquals(freeDiskFloorCommands(FreeDiskFloor.ofMebibytes(64)), engine.commands)
    }

    @Test
    fun `the suspending form reaches the engine too`() {
        val engine = FakeEngine { okReply() }
        val database = EmbeddedMongo(engine, guard(onMainThread = false))

        runBlocking { database.setFreeDiskFloor(FreeDiskFloor.ofMebibytes(64)) }

        assertEquals(freeDiskFloorCommands(FreeDiskFloor.ofMebibytes(64)), engine.commands)
    }

    /**
     * A write concern on `setParameter` would be rejected by the engine, so the command must
     * reach it exactly as it was built.
     */
    @Test
    fun `the floor commands are not given a write concern on the way out`() {
        val engine = FakeEngine { okReply() }
        val database = EmbeddedMongo(engine, guard(onMainThread = false))

        database.setFreeDiskFloorBlocking(FreeDiskFloor.ofMebibytes(64))

        assertTrue(engine.commands.none { it.containsKey("writeConcern") })
    }

    @Test
    fun `a database opened without a floor is handed back untouched`() {
        val engine = FakeEngine { okReply() }
        val database = EmbeddedMongo(engine, guard(onMainThread = false))

        assertEquals(database, database.withFreeDiskFloor(null))
        assertTrue(engine.commands.isEmpty())
    }

    @Test
    fun `a floor named at open is applied before the caller is given the database`() {
        val engine = FakeEngine { okReply() }
        val database = EmbeddedMongo(engine, guard(onMainThread = false))

        database.withFreeDiskFloor(FreeDiskFloor.ofMebibytes(64))

        assertEquals(freeDiskFloorCommands(FreeDiskFloor.ofMebibytes(64)), engine.commands)
    }

    /**
     * Only one engine may run in a process, so an open that fails after the engine started must
     * take the engine with it — otherwise nothing in this process can ever open a database again.
     */
    @Test
    fun `an engine that refuses the floor is closed rather than left running`() {
        val engine = FakeEngine { Document("ok", 0.0).append("errmsg", "no such parameter") }
        val database = EmbeddedMongo(engine, guard(onMainThread = false))

        assertFailsWith<EmbeddedMongoException> {
            database.withFreeDiskFloor(FreeDiskFloor.ofMebibytes(64))
        }

        assertEquals(1, engine.closes)
    }

    @Test
    fun `the failure the caller sees is the engine's refusal, not the close`() {
        val engine = FakeEngine { Document("ok", 0.0).append("errmsg", "no such parameter") }
        val database = EmbeddedMongo(engine, guard(onMainThread = false))

        val failure = assertFailsWith<EmbeddedMongoException> {
            database.withFreeDiskFloor(FreeDiskFloor.ofMebibytes(64))
        }

        assertEquals("no such parameter", failure.message)
        assertNull(failure.cause)
    }
}

package io.github.jeroenvervaeke.embeddedmongodb

import kotlin.test.Test
import kotlin.test.assertContains
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
        val engine = engineReporting(500)
        val database = EmbeddedMongo(engine, guard(onMainThread = false))

        database.setFreeDiskFloorBlocking(FreeDiskFloor.ofMebibytes(64))

        assertTrue(engine.databases.all { it == "admin" })
        assertEquals(
            freeDiskFloorCommands(FreeDiskFloor.ofMebibytes(64)),
            engine.commands.filter { it.keys.first() == "setParameter" },
        )
    }

    @Test
    fun `the suspending form reaches the engine too`() {
        val engine = engineReporting(500)
        val database = EmbeddedMongo(engine, guard(onMainThread = false))

        runBlocking { database.setFreeDiskFloor(FreeDiskFloor.ofMebibytes(64)) }

        assertEquals(
            freeDiskFloorCommands(FreeDiskFloor.ofMebibytes(64)),
            engine.commands.filter { it.keys.first() == "setParameter" },
        )
    }

    /**
     * A write concern on `setParameter` would be rejected by the engine, so the command must
     * reach it exactly as it was built.
     */
    @Test
    fun `the floor commands are not given a write concern on the way out`() {
        val engine = engineReporting(500)
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
        val engine = engineReporting(500)
        val database = EmbeddedMongo(engine, guard(onMainThread = false))

        database.withFreeDiskFloor(FreeDiskFloor.ofMebibytes(64))

        assertEquals(
            freeDiskFloorCommands(FreeDiskFloor.ofMebibytes(64)),
            engine.commands.filter { it.keys.first() == "setParameter" },
        )
    }

    /**
     * Only one engine may run in a process, so an open that fails after the engine started must
     * take the engine with it — otherwise nothing in this process can ever open a database again.
     */
    @Test
    fun `an engine that refuses the floor is closed rather than left running`() {
        val engine = engineReporting(500, refuse = INDEX_BUILD)
        val database = EmbeddedMongo(engine, guard(onMainThread = false))

        assertFailsWith<EmbeddedMongoException> {
            database.withFreeDiskFloor(FreeDiskFloor.ofMebibytes(64))
        }

        assertEquals(1, engine.closes)
    }

    @Test
    fun `the failure the caller sees is the engine's refusal, not the close`() {
        val engine = engineReporting(500, refuse = INDEX_BUILD)
        val database = EmbeddedMongo(engine, guard(onMainThread = false))

        val failure = assertFailsWith<EmbeddedMongoException> {
            database.withFreeDiskFloor(FreeDiskFloor.ofMebibytes(64))
        }

        assertEquals("no such parameter $INDEX_BUILD", failure.message)
        assertNull(failure.cause)
    }

    /** A close that also fails must not replace the refusal that sent the caller here. */
    @Test
    fun `a close that fails on the way out is attached to the refusal`() {
        val engine = engineReporting(
            500,
            refuse = INDEX_BUILD,
            closeFailure = IllegalStateException("the engine would not shut down"),
        )
        val database = EmbeddedMongo(engine, guard(onMainThread = false))

        val failure = assertFailsWith<EmbeddedMongoException> {
            database.withFreeDiskFloor(FreeDiskFloor.ofMebibytes(64))
        }

        assertEquals("no such parameter $INDEX_BUILD", failure.message)
        assertContains(failure.suppressed.map { it.message }, "the engine would not shut down")
    }

    /**
     * The two knobs take two commands, so a refusal on the second leaves the first already moved
     * -- and moved down, which is the direction that trades a clean refusal for an abort. A
     * caller who catches the exception must be able to believe nothing happened.
     */
    @Test
    fun `a floor the engine only half accepts is put back where it was`() {
        val engine = engineReporting(500, refuse = QUERY_SPILLING)
        val database = EmbeddedMongo(engine, guard(onMainThread = false))

        assertFailsWith<EmbeddedMongoException> {
            database.setFreeDiskFloorBlocking(FreeDiskFloor.ofMebibytes(64))
        }

        assertEquals(
            listOf(64L, 500L),
            engine.commands.mapNotNull { it[INDEX_BUILD] as? Long },
            "the index build floor was left where the failed move put it",
        )
    }

    @Test
    fun `a restore that fails too is attached to the failure rather than hiding it`() {
        val engine = engineReporting(500, refuse = QUERY_SPILLING)
        val database = EmbeddedMongo(engine, guard(onMainThread = false))

        val failure = assertFailsWith<EmbeddedMongoException> {
            database.setFreeDiskFloorBlocking(FreeDiskFloor.ofMebibytes(64))
        }

        assertTrue(failure.message.orEmpty().contains(QUERY_SPILLING), failure.message.orEmpty())
        assertEquals(1, failure.suppressed.size, "the failed restore is owed to the caller too")
    }

    @Test
    fun `a floor the engine takes is not followed by a restore`() {
        val engine = engineReporting(500)
        val database = EmbeddedMongo(engine, guard(onMainThread = false))

        database.setFreeDiskFloorBlocking(FreeDiskFloor.ofMebibytes(64))

        assertEquals(
            listOf(64L),
            engine.commands.mapNotNull { it[INDEX_BUILD] as? Long },
            "a move that worked must not be undone",
        )
    }
}

private const val INDEX_BUILD = "indexBuildMinAvailableDiskSpaceMB"

private const val QUERY_SPILLING = "internalQuerySpillingMinAvailableDiskSpaceBytes"

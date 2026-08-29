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
        assertEquals(freeDiskFloorCommands(FreeDiskFloor.ofMebibytes(64)), setParameters(engine))
    }

    @Test
    fun `the suspending form reaches the engine too`() {
        val engine = engineReporting(500)
        val database = EmbeddedMongo(engine, guard(onMainThread = false))

        runBlocking { database.setFreeDiskFloor(FreeDiskFloor.ofMebibytes(64)) }

        assertEquals(freeDiskFloorCommands(FreeDiskFloor.ofMebibytes(64)), setParameters(engine))
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
    fun `a database opened without a floor is handed back on the engine's own floors`() {
        val engine = engineReporting(500)
        val database = EmbeddedMongo(engine, guard(onMainThread = false))

        val opened = database.establishFreeDiskFloor(null, EngineFloorDefaults())

        assertEquals(database, opened)
        assertEquals(floorCommands(500L, 524_288_000L), setParameters(engine))
    }

    /** Counted rather than merely checked, so an open that sends nothing cannot satisfy this. */
    @Test
    fun `every command an open sends to establish the floor goes to the admin database`() {
        val engine = engineReporting(500)

        EmbeddedMongo(engine, guard(onMainThread = false))
            .establishFreeDiskFloor(null, EngineFloorDefaults())

        assertEquals(listOf("admin", "admin", "admin"), engine.databases)
    }

    /**
     * The defect the open path exists for. The floors are server parameters and the engine is one
     * runtime per process, so a floor a database lowered is still in force after its close. A
     * caller who names none must be given MongoDB's floors, not the last database's.
     */
    @Test
    fun `a floor left behind by a closed database does not reach the next open`() {
        val defaults = EngineFloorDefaults()
        val lowered = engineReporting(500)
        EmbeddedMongo(lowered, guard(onMainThread = false))
            .establishFreeDiskFloor(FreeDiskFloor.ofMebibytes(32), defaults)

        // The next database opens on an engine still holding the 32 MiB the first one set.
        val next = engineReporting(32)
        EmbeddedMongo(next, guard(onMainThread = false)).establishFreeDiskFloor(null, defaults)

        assertEquals(floorCommands(500L, 524_288_000L), setParameters(next))
    }

    /**
     * The first open in a process may be one that names a floor, so the defaults have to be
     * recorded before that floor is applied. Recording them afterwards would take the caller's
     * floor for MongoDB's and hand it to every later open that asked for the default — the same
     * defect as inheriting one, moved a step earlier.
     */
    @Test
    fun `the floors recorded are the ones from before the first caller moved them`() {
        val defaults = EngineFloorDefaults()
        val engine = engineReporting(500)

        EmbeddedMongo(engine, guard(onMainThread = false))
            .establishFreeDiskFloor(FreeDiskFloor.ofMebibytes(32), defaults)

        assertEquals(ReportedFloors(500L, 524_288_000L), defaults.of(neverRead()))
    }

    /** The restore is a command like any other, and the engine may refuse it like any other. */
    @Test
    fun `an engine that refuses the floors put back at open is closed too`() {
        val engine = engineReporting(500, refuse = INDEX_BUILD)

        assertFailsWith<EmbeddedMongoException> {
            EmbeddedMongo(engine, guard(onMainThread = false))
                .establishFreeDiskFloor(null, EngineFloorDefaults())
        }

        assertEquals(1, engine.closes)
    }

    @Test
    fun `the engine's own floors are read once and not asked for again`() {
        val defaults = EngineFloorDefaults()
        EmbeddedMongo(engineReporting(500), guard(onMainThread = false))
            .establishFreeDiskFloor(null, defaults)

        // Answers the restore but refuses to be read: by now the defaults must be remembered, and
        // re-reading them here would be reading the floor the first database left behind.
        val next = FakeEngine { command ->
            check(command.keys.first() != "getParameter") { "the defaults were read a second time" }
            okReply()
        }
        EmbeddedMongo(next, guard(onMainThread = false)).establishFreeDiskFloor(null, defaults)

        assertEquals(floorCommands(500L, 524_288_000L), next.commands)
    }

    /**
     * The knobs are read back separately and can disagree, and the spilling one is a byte count
     * that need not be a whole mebibyte — so an open replays what was read rather than a floor
     * rounded through [FreeDiskFloor].
     */
    @Test
    fun `floors the engine reported separately are put back separately`() {
        val engine = FakeEngine { command ->
            if (command.keys.first() == "getParameter") {
                okReply(INDEX_BUILD to 500L, QUERY_SPILLING to 123_456_789L)
            } else {
                okReply()
            }
        }

        EmbeddedMongo(engine, guard(onMainThread = false))
            .establishFreeDiskFloor(null, EngineFloorDefaults())

        assertEquals(floorCommands(500L, 123_456_789L), setParameters(engine))
    }

    /**
     * A knob this library cannot find is a floor it cannot promise. Failing the open is the loud
     * end of that trade; the silent end is an index build refused on a device months later.
     */
    @Test
    fun `an open whose floors cannot be read takes the engine with it`() {
        val engine = FakeEngine { okReply(INDEX_BUILD to 500L) }
        val database = EmbeddedMongo(engine, guard(onMainThread = false))

        assertFailsWith<EmbeddedMongoException> {
            database.establishFreeDiskFloor(null, EngineFloorDefaults())
        }

        assertEquals(1, engine.closes)
    }

    @Test
    fun `a floor named at open is applied before the caller is given the database`() {
        val engine = engineReporting(500)
        val database = EmbeddedMongo(engine, guard(onMainThread = false))

        database.establishFreeDiskFloor(FreeDiskFloor.ofMebibytes(64), EngineFloorDefaults())

        assertEquals(freeDiskFloorCommands(FreeDiskFloor.ofMebibytes(64)), setParameters(engine))
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
            database.establishFreeDiskFloor(FreeDiskFloor.ofMebibytes(64), EngineFloorDefaults())
        }

        assertEquals(1, engine.closes)
    }

    @Test
    fun `the failure the caller sees is the engine's refusal, not the close`() {
        val engine = engineReporting(500, refuse = INDEX_BUILD)
        val database = EmbeddedMongo(engine, guard(onMainThread = false))

        val failure = assertFailsWith<EmbeddedMongoException> {
            database.establishFreeDiskFloor(FreeDiskFloor.ofMebibytes(64), EngineFloorDefaults())
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
            database.establishFreeDiskFloor(FreeDiskFloor.ofMebibytes(64), EngineFloorDefaults())
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

private fun setParameters(engine: FakeEngine): List<Document> =
    engine.commands.filter { it.keys.first() == "setParameter" }

/**
 * A database that fails the test if the defaults are read from it, for asserting what was
 * recorded earlier without the read itself supplying the answer.
 */
private fun neverRead(): EmbeddedMongo = EmbeddedMongo(
    FakeEngine { error("the defaults were read here rather than before the floor moved") },
    guard(onMainThread = false),
)

/**
 * The two `setParameter` commands carrying these floors, spelled out here rather than built by
 * the code under test — an expectation the production helper produced would agree with itself.
 */
private fun floorCommands(mebibytes: Long, bytes: Long): List<Document> = listOf(
    Document("setParameter", 1).append(INDEX_BUILD, mebibytes),
    Document("setParameter", 1).append(QUERY_SPILLING, bytes),
)

private const val INDEX_BUILD = "indexBuildMinAvailableDiskSpaceMB"

private const val QUERY_SPILLING = "internalQuerySpillingMinAvailableDiskSpaceBytes"

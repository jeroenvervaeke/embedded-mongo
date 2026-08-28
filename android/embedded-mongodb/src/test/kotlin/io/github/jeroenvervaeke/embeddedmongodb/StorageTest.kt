package io.github.jeroenvervaeke.embeddedmongodb

import kotlin.test.Test
import kotlin.test.assertContains
import kotlin.test.assertEquals
import kotlin.test.assertFailsWith

class StorageTest {
    @Test
    fun `a volume that cannot give the engine room is refused before the engine is asked`() {
        val failure = assertFailsWith<InsufficientStorageException> {
            checkAllocatable(64L * MEGABYTE, floor = null)
        }

        assertEquals(64L * MEGABYTE, failure.allocatableBytes)
        assertEquals(256L * MEGABYTE, failure.requiredBytes)
    }

    @Test
    fun `the refusal says how much room there is and how much is wanted`() {
        val failure = assertFailsWith<InsufficientStorageException> {
            checkAllocatable(64L * MEGABYTE, floor = null)
        }

        assertContains(failure.message.orEmpty(), "256 MB")
        assertContains(failure.message.orEmpty(), "64 MB")
    }

    @Test
    fun `a volume with room to spare is accepted`() {
        checkAllocatable(4L * 1024 * MEGABYTE, floor = null)
    }

    @Test
    fun `the floor itself is enough`() {
        checkAllocatable(256L * MEGABYTE, floor = null)
    }

    @Test
    fun `the floor sits below what the engine asks of the filesystem`() {
        // getAllocatableBytes answers for this application, not for the volume, and is the
        // smaller number. Holding it to the engine's own 500 MB would refuse devices the engine
        // opens on happily.
        checkAllocatable(450L * MEGABYTE, floor = null)
    }

    /**
     * An application that lowered the engine's floor to work on a nearly-full device must not
     * then be refused by this library's own precondition, which would take the knob straight
     * back.
     */
    @Test
    fun `a caller who lowered the engine's floor is held to that instead`() {
        checkAllocatable(64L * MEGABYTE, floor = FreeDiskFloor.ofMebibytes(32))
    }

    @Test
    fun `a lowered floor still refuses a volume that cannot even meet it`() {
        val failure = assertFailsWith<InsufficientStorageException> {
            checkAllocatable(16L * MEGABYTE, floor = FreeDiskFloor.ofMebibytes(32))
        }

        assertEquals(32L * MEGABYTE, failure.requiredBytes)
    }

    /**
     * Raising the engine's floor says nothing about how much the platform will hand this
     * application, which is the smaller number this check is against.
     */
    @Test
    fun `raising the engine's floor does not raise what this check asks for`() {
        assertEquals(
            256L * MEGABYTE,
            requiredFreeBytes(FreeDiskFloor.ofMebibytes(4096)),
        )
    }

    @Test
    fun `the floor the engine defaults to leaves this check where it was`() {
        assertEquals(256L * MEGABYTE, requiredFreeBytes(FreeDiskFloor.ENGINE_DEFAULT))
        assertEquals(256L * MEGABYTE, requiredFreeBytes(floor = null))
    }
}

private const val MEGABYTE = 1024L * 1024L

package io.github.jeroenvervaeke.embeddedmongodb

import kotlin.test.Test
import kotlin.test.assertContains
import kotlin.test.assertEquals
import kotlin.test.assertFailsWith

class StorageTest {
    @Test
    fun `a volume that cannot give the engine room is refused before the engine is asked`() {
        val failure = assertFailsWith<InsufficientStorageException> {
            checkAllocatable(64L * MEGABYTE)
        }

        assertEquals(64L * MEGABYTE, failure.allocatableBytes)
        assertEquals(256L * MEGABYTE, failure.requiredBytes)
    }

    @Test
    fun `the refusal says how much room there is and how much is wanted`() {
        val failure = assertFailsWith<InsufficientStorageException> {
            checkAllocatable(64L * MEGABYTE)
        }

        assertContains(failure.message.orEmpty(), "256 MB")
        assertContains(failure.message.orEmpty(), "64 MB")
    }

    @Test
    fun `a volume with room to spare is accepted`() {
        checkAllocatable(4L * 1024 * MEGABYTE)
    }

    @Test
    fun `the floor itself is enough`() {
        checkAllocatable(256L * MEGABYTE)
    }

    @Test
    fun `the floor sits below what the engine asks of the filesystem`() {
        // getAllocatableBytes answers for this application, not for the volume, and is the
        // smaller number. Holding it to the engine's own 500 MB would refuse devices the engine
        // opens on happily.
        checkAllocatable(450L * MEGABYTE)
    }
}

private const val MEGABYTE = 1024L * 1024L

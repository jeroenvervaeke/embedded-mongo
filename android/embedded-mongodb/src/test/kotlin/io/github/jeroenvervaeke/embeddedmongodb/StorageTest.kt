package io.github.jeroenvervaeke.embeddedmongodb

import kotlin.test.Test
import kotlin.test.assertContains
import kotlin.test.assertEquals
import kotlin.test.assertFailsWith

class StorageTest {
    @Test
    fun `a volume that cannot give the engine room is refused before the engine is asked`() {
        val failure = assertFailsWith<InsufficientStorageException> {
            checkAllocatable(64L * MEGABYTE, StorageOptions())
        }

        assertEquals(64L * MEGABYTE, failure.allocatableBytes)
        assertEquals(256L * MEGABYTE, failure.requiredBytes)
    }

    @Test
    fun `the refusal says how much room there is and how much is wanted`() {
        val failure = assertFailsWith<InsufficientStorageException> {
            checkAllocatable(64L * MEGABYTE, StorageOptions())
        }

        assertContains(failure.message.orEmpty(), "256 MiB")
        assertContains(failure.message.orEmpty(), "64 MiB")
    }

    @Test
    fun `a volume with room to spare is accepted`() {
        checkAllocatable(4L * 1024 * MEGABYTE, StorageOptions())
    }

    @Test
    fun `the floor itself is enough`() {
        checkAllocatable(256L * MEGABYTE, StorageOptions())
    }

    @Test
    fun `the floor sits below what the engine asks of the filesystem`() {
        // getAllocatableBytes answers for this application, not for the volume, and is the
        // smaller number. Holding it to the engine's own 500 MB would refuse devices the engine
        // opens on happily.
        checkAllocatable(450L * MEGABYTE, StorageOptions())
    }

    /**
     * An application that lowered the engine's floor to work on a nearly-full device must not
     * then be refused by this library's own precondition, which would take the knob straight
     * back.
     */
    @Test
    fun `a caller who lowered the engine's floor is held to that instead`() {
        checkAllocatable(64L * MEGABYTE, floorOf(32))
    }

    @Test
    fun `a lowered floor still refuses a volume that cannot even meet it`() {
        val failure = assertFailsWith<InsufficientStorageException> {
            checkAllocatable(16L * MEGABYTE, floorOf(32))
        }

        assertEquals(32L * MEGABYTE, failure.requiredBytes)
    }

    /**
     * Raising the engine's floor says nothing about how much the platform will hand this
     * application, which is the smaller number this check is against.
     */
    @Test
    fun `raising the engine's floor does not raise what this check asks for`() {
        assertEquals(256L * MEGABYTE, requiredFreeBytes(floorOf(4096)))
    }

    @Test
    fun `the floor the engine defaults to leaves this check where it was`() {
        assertEquals(256L * MEGABYTE, requiredFreeBytes(StorageOptions(freeDiskFloor = FreeDiskFloor.ENGINE_DEFAULT)))
        assertEquals(256L * MEGABYTE, requiredFreeBytes(StorageOptions()))
    }

    /**
     * The floor governs index builds, not whether WiredTiger can create its first journal file.
     * A floor below what opening costs must not drag this check down with it: the engine would
     * then start on a volume it cannot finish opening on, and running out there aborts the
     * process rather than failing a call.
     */
    @Test
    fun `a floor below what opening costs does not lower this check below it`() {
        assertEquals(9L * MEGABYTE, requiredFreeBytes(floorOf(1)))
    }

    @Test
    fun `a volume that cannot fit the journal is refused however low the floor is`() {
        val failure = assertFailsWith<InsufficientStorageException> {
            checkAllocatable(2L * MEGABYTE, floorOf(1))
        }

        assertEquals(9L * MEGABYTE, failure.requiredBytes)
    }

    @Test
    fun `a larger journal raises what opening is known to cost`() {
        val options = StorageOptions(
            journalFileSize = JournalFileSize.ofKibibytes(64 * 1024),
            freeDiskFloor = FreeDiskFloor.ofMebibytes(1),
        )

        assertEquals(65L * MEGABYTE, requiredFreeBytes(options))
    }

    /** A spare journal file is a second one on disk from the moment the engine starts. */
    @Test
    fun `keeping a spare journal file doubles what opening is known to cost`() {
        val options = StorageOptions(
            journalPreallocation = JournalPreallocation.ENABLED,
            freeDiskFloor = FreeDiskFloor.ofMebibytes(1),
        )

        assertEquals(17L * MEGABYTE, requiredFreeBytes(options))
    }

    @Test
    fun `the default check still sits above what opening costs`() {
        assertEquals(256L * MEGABYTE, requiredFreeBytes(StorageOptions()))
    }
}

private fun floorOf(mebibytes: Int) =
    StorageOptions(freeDiskFloor = FreeDiskFloor.ofMebibytes(mebibytes))

private const val MEGABYTE = 1024L * 1024L

package io.github.jeroenvervaeke.embeddedmongodb

import kotlin.test.Test
import kotlin.test.assertContentEquals
import kotlin.test.assertEquals
import kotlin.test.assertFailsWith
import kotlin.test.assertTrue

class StorageOptionsTest {
    @Test
    fun `an untouched options object names no limit at all`() {
        assertContentEquals(longArrayOf(), StorageOptions().engineSlots())
    }

    @Test
    fun `every limit reaches its own slot in the unit the engine takes`() {
        val options = StorageOptions(
            cacheSize = CacheSize.ofMebibytes(64),
            journalFileSize = JournalFileSize.ofKibibytes(512),
            journalPreallocation = JournalPreallocation.ENABLED,
        )

        assertContentEquals(longArrayOf(64, 512, 1), options.engineSlots())
    }

    @Test
    fun `disabled pre-allocation is a value of its own, not the absence of one`() {
        val options = StorageOptions(journalPreallocation = JournalPreallocation.DISABLED)

        assertContentEquals(longArrayOf(0, 0, 2), options.engineSlots())
    }

    @Test
    fun `a limit nobody named is sent as zero rather than as a number chosen here`() {
        val options = StorageOptions(journalPreallocation = JournalPreallocation.ENABLED)

        assertContentEquals(longArrayOf(0, 0, 1), options.engineSlots())
    }

    @Test
    fun `the slots past the last one named are left off the end`() {
        val options = StorageOptions(cacheSize = CacheSize.ofMebibytes(32))

        assertContentEquals(longArrayOf(32), options.engineSlots())
    }

    @Test
    fun `the free disk floor is not one of the slots, because it is set with a command`() {
        val options = StorageOptions(freeDiskFloor = FreeDiskFloor.ofMebibytes(64))

        assertContentEquals(longArrayOf(), options.engineSlots())
    }

    @Test
    fun `a cache below WiredTiger's minimum is refused at the line that wrote it`() {
        val failure = assertFailsWith<IllegalArgumentException> { CacheSize.ofMebibytes(0) }

        assertEquals("cache size must be between 1 and 10000000 MiB, got 0", failure.message)
    }

    @Test
    fun `a cache above WiredTiger's maximum is refused`() {
        assertFailsWith<IllegalArgumentException> {
            CacheSize.ofMebibytes(CacheSize.MAX_MEBIBYTES + 1)
        }
    }

    @Test
    fun `a journal file below WiredTiger's minimum is refused`() {
        assertFailsWith<IllegalArgumentException> {
            JournalFileSize.ofKibibytes(JournalFileSize.MIN_KIBIBYTES - 1)
        }
    }

    @Test
    fun `a journal file above WiredTiger's maximum is refused`() {
        assertFailsWith<IllegalArgumentException> {
            JournalFileSize.ofKibibytes(JournalFileSize.MAX_KIBIBYTES + 1)
        }
    }

    @Test
    fun `a negative limit is refused rather than sent as a slot the bridge cannot read`() {
        assertFailsWith<IllegalArgumentException> { CacheSize.ofMebibytes(-1) }
        assertFailsWith<IllegalArgumentException> { JournalFileSize.ofKibibytes(-1) }
    }

    @Test
    fun `the boundary values themselves are accepted`() {
        assertEquals(1, CacheSize.ofMebibytes(CacheSize.MIN_MEBIBYTES).mebibytes)
        assertEquals(10_000_000, CacheSize.ofMebibytes(CacheSize.MAX_MEBIBYTES).mebibytes)
        assertEquals(100, JournalFileSize.ofKibibytes(JournalFileSize.MIN_KIBIBYTES).kibibytes)
        assertEquals(
            2 * 1024 * 1024,
            JournalFileSize.ofKibibytes(JournalFileSize.MAX_KIBIBYTES).kibibytes,
        )
    }

    /**
     * Zero is what the bridge reads as "the caller named nothing", so no policy may encode as
     * it — a pre-allocation that did would be silently discarded.
     */
    @Test
    fun `no pre-allocation policy encodes as the value that means unset`() {
        assertTrue(JournalPreallocation.entries.none { it.slot == 0L })
    }
}

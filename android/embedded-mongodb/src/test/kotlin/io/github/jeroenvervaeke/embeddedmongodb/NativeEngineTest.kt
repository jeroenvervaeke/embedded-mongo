package io.github.jeroenvervaeke.embeddedmongodb

import java.io.File
import kotlin.test.Test
import kotlin.test.assertContentEquals
import kotlin.test.assertEquals
import kotlin.test.assertNull

/**
 * Which native entry point an open reaches, which is a compatibility promise rather than an
 * implementation detail: an application that names no limit must not start depending on a symbol
 * that older builds of the native library never exported.
 */
class NativeEngineTest {
    @Test
    fun `a caller who names no limit reaches the entry point that predates them`() {
        val opener = FakeOpener()

        NativeEngine.open(File("/data/shop"), StorageOptions(), opener)

        assertEquals("/data/shop", opener.openedPath)
        assertNull(opener.openedWithOptionsPath, "openWithOptions must not be reached")
    }

    @Test
    fun `a caller who names a limit reaches the entry point that carries it`() {
        val opener = FakeOpener()
        val options = StorageOptions(cacheSize = CacheSize.ofMebibytes(64))

        NativeEngine.open(File("/data/shop"), options, opener)

        assertEquals("/data/shop", opener.openedWithOptionsPath)
        assertContentEquals(longArrayOf(64), opener.slots)
        assertNull(opener.openedPath, "the entry point without options must not be reached")
    }

    /**
     * The floor is set with a command after the engine is up, so naming one alone must not send
     * the open down the new entry point.
     */
    @Test
    fun `a caller who names only the free disk floor still reaches the older entry point`() {
        val opener = FakeOpener()
        val options = StorageOptions(freeDiskFloor = FreeDiskFloor.ofMebibytes(64))

        NativeEngine.open(File("/data/shop"), options, opener)

        assertEquals("/data/shop", opener.openedPath)
        assertNull(opener.openedWithOptionsPath, "the floor needs no native entry point")
    }

    @Test
    fun `the path the engine is given is absolute, because it resolves relative ones elsewhere`() {
        val opener = FakeOpener()

        NativeEngine.open(File("shop"), StorageOptions(), opener)

        assertEquals(File("shop").absolutePath, opener.openedPath)
    }
}

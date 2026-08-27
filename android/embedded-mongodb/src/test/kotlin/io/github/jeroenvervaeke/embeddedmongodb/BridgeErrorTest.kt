package io.github.jeroenvervaeke.embeddedmongodb

import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertNull

class BridgeErrorTest {
    @Test
    fun `every code the bridge reports is named`() {
        val named = (-5..-1).map { BridgeError.of(it) }

        assertEquals(BridgeError.entries.sortedBy(BridgeError::code), named.filterNotNull())
    }

    @Test
    fun `a closed or forged handle is told apart from an engine failure`() {
        val closed = EmbeddedMongoException("no such handle", BridgeError.UNKNOWN_HANDLE.code)

        assertEquals(BridgeError.UNKNOWN_HANDLE, closed.bridgeError)
    }

    @Test
    fun `a MongoDB code is not a bridge error`() {
        assertNull(EmbeddedMongoException("duplicate key", 11000).bridgeError)
    }

    @Test
    fun `a failure raised on this side of the bridge is not a bridge error`() {
        assertNull(EmbeddedMongoException("malformed reply", NO_ERROR_CODE).bridgeError)
    }
}

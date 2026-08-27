package io.github.jeroenvervaeke.embeddedmongodb

import kotlin.test.Test
import kotlin.test.assertContains
import kotlin.test.assertEquals
import kotlin.test.assertFailsWith
import kotlin.test.assertTrue

class MainThreadGuardTest {
    @Test
    fun `rejecting on the main thread names the operation and the way out`() {
        val failure = assertFailsWith<IllegalStateException> {
            guard(onMainThread = true).reject("Running a MongoDB command")
        }

        val message = failure.message.orEmpty()
        assertContains(message, "Running a MongoDB command")
        assertContains(message, "suspending")
    }

    @Test
    fun `a background thread is left alone`() {
        guard(onMainThread = false).reject("Running a MongoDB command")
    }

    @Test
    fun `warning on the main thread reports instead of throwing`() {
        val reported = mutableListOf<String>()

        guard(onMainThread = true, report = reported::add).warn("Closing a database")

        assertEquals(1, reported.size)
        assertContains(reported.single(), "Closing a database")
    }

    @Test
    fun `warning off the main thread reports nothing`() {
        val reported = mutableListOf<String>()

        guard(onMainThread = false, report = reported::add).warn("Closing a database")

        assertTrue(reported.isEmpty())
    }
}

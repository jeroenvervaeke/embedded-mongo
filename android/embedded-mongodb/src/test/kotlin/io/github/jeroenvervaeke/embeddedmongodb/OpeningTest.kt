package io.github.jeroenvervaeke.embeddedmongodb

import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertTrue
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch
import kotlinx.coroutines.runBlocking

/**
 * That an open which finishes after its caller has gone does not leave an engine behind. Only one
 * runtime may exist per process, so a database nobody holds a handle to is one this process can
 * never replace.
 */
class OpeningTest {
    @Test
    fun `a caller who is still there is handed the database`() {
        val engine = FakeEngine { okReply() }

        val database = runBlocking { openedOrClosed { EmbeddedMongo(engine, guard(onMainThread = false)) } }

        assertEquals(0, engine.closes, "a database on its way to a live caller must not be closed")
        database.close()
    }

    @Test
    fun `a caller cancelled while the engine was starting does not leave it open`() {
        val engine = FakeEngine { okReply() }
        val opening = CountDownLatch(1)
        val cancelled = CountDownLatch(1)

        runBlocking {
            val job = launch(Dispatchers.Default) {
                openedOrClosed {
                    opening.countDown()
                    // Held open until the caller has been cancelled, which is the race: the engine
                    // is starting and there is no suspension point at which to interrupt it.
                    assertTrue(cancelled.await(PATIENCE_SECONDS, TimeUnit.SECONDS))
                    EmbeddedMongo(engine, guard(onMainThread = false))
                }
            }
            assertTrue(opening.await(PATIENCE_SECONDS, TimeUnit.SECONDS))
            job.cancel()
            cancelled.countDown()
            job.join()
        }

        assertEquals(1, engine.closes, "the engine was left running after the caller had gone")
    }

    /** The open runs to completion whatever the caller does, because nothing can interrupt it. */
    @Test
    fun `cancelling does not stop the open that is already running`() {
        val engine = FakeEngine { okReply() }
        val opening = CountDownLatch(1)
        val cancelled = CountDownLatch(1)
        var finished = false

        runBlocking {
            val job = launch(Dispatchers.Default) {
                openedOrClosed {
                    opening.countDown()
                    assertTrue(cancelled.await(PATIENCE_SECONDS, TimeUnit.SECONDS))
                    finished = true
                    EmbeddedMongo(engine, guard(onMainThread = false))
                }
            }
            assertTrue(opening.await(PATIENCE_SECONDS, TimeUnit.SECONDS))
            job.cancel()
            cancelled.countDown()
            job.join()
        }

        assertTrue(finished, "the open must run to completion so that its engine can be closed")
    }
}

/** Long enough that a loaded machine cannot mistake scheduling for a deadlock. */
private const val PATIENCE_SECONDS = 60L

"""That cancelling a task stops the engine, and not merely the waiting.

The distinction is the reason to build an async binding rather than only a non-blocking one.
A cancellation that stops the wait leaves the session held for as long as the query would have
run, so a handful of abandoned slow queries starve the pool and every later command queues
behind them. A cancellation that reaches the engine gives the session straight back.

`tests/cancellation.rs` measures this for the Rust API. This measures whether it survives the
trip through pyo3's coroutines and PyMongo's pool -- which it only does if cancelling the task
really drops the Rust future rather than detaching it and letting it run.
"""

import asyncio
import time
import unittest

from support import collection_name, crunch, scratch

from pymongo_embedded import AsyncMongoClient

# Grinds started at once. Every session in the default pool, so that a command issued afterwards
# has to wait for one of them: with the engine saturated there is nowhere for a ping to hide.
GRINDS = 8

# One more connection than there are grinds, so PyMongo can always hand the ping one. What the
# ping must then wait for is a session, which is the thing under test.
CONNECTIONS = GRINDS + 1

DOCUMENTS = 1500

# Terms summed per document. Sized so one grind runs for well over `PATIENCE`, which is what
# makes "did it stop early?" a question with a clear answer.
STEPS = 6000

# How long the grinds are allowed to run before they are abandoned.
PATIENCE = 0.3


class AsyncCancellationMeasurement(unittest.IsolatedAsyncioTestCase):
    async def test_cancelling_the_tasks_frees_their_sessions(self):
        with scratch() as directory:
            client = AsyncMongoClient(
                f"mongodb_embedded://{directory}", maxPoolSize=CONNECTIONS
            )
            try:
                await _seed(client)

                # What one grind really costs, so the assertions compare against a measurement
                # rather than a guess.
                started = time.perf_counter()
                await _crunch(client.concurrency, 0)
                uninterrupted = time.perf_counter() - started
                self.assertGreater(
                    uninterrupted,
                    PATIENCE * 2,
                    f"a grind finishes in {uninterrupted:.3f}s, too fast for a {PATIENCE}s "
                    f"patience to prove anything -- raise STEPS",
                )

                cancelled = await self._ping_after_abandoning(client, shielded=False)
                # The control. `shield` stops the cancellation reaching the command, so the
                # engine works on. If the reading above were an artefact -- a spare session, a
                # ping that never needed one -- this would come back just as fast.
                controlled = await self._ping_after_abandoning(client, shielded=True)
            finally:
                await client.close()

        print(
            f"grind {uninterrupted:.3f}s; ping after cancelling {cancelled:.3f}s; "
            f"after abandoning without cancelling {controlled:.3f}s"
        )
        self.assertLess(
            cancelled,
            uninterrupted / 2,
            f"the ping waited {cancelled:.3f}s against a {uninterrupted:.3f}s grind, so the "
            f"cancelled commands were still holding their sessions -- the cancellation stopped "
            f"the waiting and not the engine",
        )
        self.assertGreater(
            controlled,
            uninterrupted / 2,
            f"the ping waited only {controlled:.3f}s while {GRINDS} un-cancelled grinds held "
            f"every session, so it never needed one and the reading above measures nothing",
        )

    async def _ping_after_abandoning(self, client, shielded):
        """Saturates every session with grinds, abandons them, and answers how long a ping then
        waited.

        `shielded` is what makes this both the measurement and its own control: abandoning a
        shielded task stops this waiting for the command while leaving the command itself
        running, which is exactly the behaviour a binding that did not cancel would have.
        """
        grinds = [
            asyncio.ensure_future(_crunch(client.concurrency, index))
            for index in range(GRINDS)
        ]
        abandoned = [asyncio.shield(grind) for grind in grinds] if shielded else grinds

        await asyncio.sleep(PATIENCE)
        for task in abandoned:
            task.cancel()
        await asyncio.gather(*abandoned, return_exceptions=True)

        started = time.perf_counter()
        await client.admin.command("ping")
        waited = time.perf_counter() - started

        # The shielded grinds are still running and still hold sessions; nothing later in this
        # test may run until they are done, or it would be measuring their leftovers.
        await asyncio.gather(*grinds, return_exceptions=True)
        return waited


async def _seed(client):
    for index in range(GRINDS):
        await client.concurrency[collection_name(index)].insert_many(
            [{"n": n} for n in range(DOCUMENTS)]
        )


async def _crunch(database, index):
    cursor = await database[collection_name(index)].aggregate(crunch(STEPS))
    return len([document async for document in cursor])


if __name__ == "__main__":
    unittest.main()

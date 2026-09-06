"""That N asyncio tasks run N commands at once, and that the loop keeps running meanwhile.

Two properties, measured rather than asserted about, as `tests/session_concurrency.rs` and
test_concurrency.py do for the synchronous surface.

The first is the same claim the synchronous binding makes and is here to catch the same
mistake: a lock held across a command in the binding would serialise everything the engine runs
in parallel, whatever the pool below is sized to.

The second is the one an async binding exists for and the synchronous one cannot have at all.
It releases the interpreter, so other OS threads run -- but an asyncio application has one
thread that matters, and that is the one a blocking command occupies.
"""

import asyncio
import os
import time
import unittest

from support import collection_name, crunch, scratch

from pymongo_embedded import AsyncMongoClient

# Commands per round, and tasks in the parallel round. Matched to the session pool's own default
# of eight, so the engine is not the ceiling this measures.
COMMANDS = 8

# Documents per collection, each of which the pipeline crunches.
DOCUMENTS = 1500

# Terms summed per document, for the rounds that are being timed.
STEPS = 1200

# Terms for the single command the loop test runs alongside its ticker. Long enough that the
# ticker's rate over it is a rate and not a sample of one.
SLOW_STEPS = 6000

# How long the idle reference rate is measured over.
LEAD = 0.2


class AsyncConcurrencyTest(unittest.IsolatedAsyncioTestCase):
    async def test_tasks_run_commands_in_parallel(self):
        cores = os.cpu_count() or 1
        if cores < COMMANDS:
            # Below one core per command the machine, not the binding, is the gate, and the
            # ratio this asserts would be measuring the scheduler.
            self.skipTest(f"{COMMANDS} concurrent commands need {COMMANDS} cores, found {cores}")

        with scratch() as directory:
            client = AsyncMongoClient(f"mongodb_embedded://{directory}", maxPoolSize=COMMANDS)
            try:
                await _seed(client, COMMANDS)
                # Untimed: this pays for the connections and for WiredTiger's first read of
                # each collection, neither of which the rounds below should be measuring.
                await self._round(client, parallel=True)
                serial = await self._round(client, parallel=False)
                parallel = await self._round(client, parallel=True)
            finally:
                await client.close()

        speedup = serial / parallel
        print(f"{COMMANDS} commands, {cores} cores: {serial:.3f}s one after another, "
              f"{parallel:.3f}s gathered ({speedup:.2f}x)")

        floor = COMMANDS / 2
        self.assertGreaterEqual(
            speedup,
            floor,
            f"{COMMANDS} commands gathered ran {speedup:.2f}x faster than awaited one after "
            f"another, short of the {floor:.2f}x tasks that really run in parallel would give: "
            f"{parallel:.3f}s against {serial:.3f}s",
        )

    async def test_the_event_loop_runs_while_a_command_does(self):
        with scratch() as directory:
            client = AsyncMongoClient(f"mongodb_embedded://{directory}", maxPoolSize=COMMANDS)
            try:
                await _seed(client, 1)
                # The reference: what an unrelated task gets when nothing is competing for the
                # loop at all. Measured on this machine rather than assumed, because how fast a
                # task can round-trip through the loop is a property of the machine.
                idle = await _rate(asyncio.sleep(LEAD))
                busy = await _rate(_crunch(client.concurrency, 0, SLOW_STEPS))
            finally:
                await client.close()

        # Half the idle rate. A binding that ran the command on the loop thread scores near
        # zero: no other task gets to run at all until the command is done.
        self.assertGreater(
            busy,
            idle / 2,
            f"an unrelated task ran at {busy:,.0f} turns/s while a command was in flight, "
            f"against {idle:,.0f} with the loop otherwise idle -- the command is occupying the "
            f"event loop rather than a worker thread",
        )

    async def _round(self, client, parallel):
        """Runs [`COMMANDS`] aggregations, gathered or one after another, and answers how long
        they took."""
        database = client.concurrency
        commands = [_crunch(database, index, STEPS) for index in range(COMMANDS)]
        started = time.perf_counter()
        if parallel:
            groups = list(await asyncio.gather(*commands))
        else:
            groups = [await command for command in commands]
        elapsed = time.perf_counter() - started

        self.assertEqual(
            [1] * COMMANDS, groups, "every aggregation should answer exactly one group"
        )
        return elapsed


async def _rate(awaitable):
    """How many turns a second an unrelated task got while `awaitable` was pending."""
    stop = asyncio.Event()
    ticker = asyncio.create_task(_turn(stop))
    started = time.perf_counter()
    await awaitable
    elapsed = time.perf_counter() - started
    stop.set()
    turns = await ticker
    return turns / elapsed


async def _turn(stop):
    """A task that does nothing but hand control back, so a stretch in which the loop never ran
    anything else shows up as a count that did not move."""
    turns = 0
    while not stop.is_set():
        turns += 1
        await asyncio.sleep(0)
    return turns


async def _seed(client, collections):
    for index in range(collections):
        await client.concurrency[collection_name(index)].insert_many(
            [{"n": n} for n in range(DOCUMENTS)]
        )


async def _crunch(database, index, steps):
    """One command's work, and how many groups it answered."""
    cursor = await database[collection_name(index)].aggregate(crunch(steps))
    return len([document async for document in cursor])


if __name__ == "__main__":
    unittest.main()

"""That N Python threads run N commands at once.

The layering is what is under test, not the engine: PyMongo hands out one connection per
concurrent operation, every connection reaches the same `NativeClient`, and that object holds
one lock across the whole command. A lock taken exclusively there makes the binding the ceiling
-- eight threads and eight sessions still give one command at a time, whatever the pool below is
sized to.

Deliberately a wall-clock measurement rather than an assertion about locks, for the same reason
`tests/session_concurrency.rs` is: it is the property callers actually have, and it fails
whatever reintroduces serialisation, in Python or in Rust.
"""

import os
import time
import unittest
from concurrent.futures import ThreadPoolExecutor

from support import collection_name as _collection, crunch, scratch

from pymongo_embedded import MongoClient

# Commands per round, and threads in the parallel round. Matched to the session pool's own
# default of eight, so the engine is not the ceiling this measures.
COMMANDS = 8

# Documents per collection, each of which the pipeline below crunches.
DOCUMENTS = 1500

# Terms summed per document. With the document count above this puts one command in the tens of
# milliseconds -- long enough to dwarf PyMongo's own per-command work, short enough that three
# rounds of it stay well inside a test's patience.
STEPS = 1200


class ThreadConcurrencyMeasurement(unittest.TestCase):
    def test_threads_run_commands_in_parallel(self):
        cores = os.cpu_count() or 1
        if cores < COMMANDS:
            # Below one core per command the machine, not the binding, is the gate, and the
            # ratio this asserts would be measuring the scheduler.
            self.skipTest(f"{COMMANDS} concurrent commands need {COMMANDS} cores, found {cores}")

        with scratch() as directory:
            # maxPoolSize is the other ceiling: PyMongo hands out one connection per concurrent
            # operation, so a pool smaller than the round would serialise it in Python before
            # anything reached Rust.
            with MongoClient(f"mongodb_embedded://{directory}", maxPoolSize=COMMANDS) as client:
                _seed(client, COMMANDS)
                # Untimed: this is what pays for the connections, the cursor machinery and
                # WiredTiger's first read of each collection, none of which the rounds below
                # should be measuring.
                self._round(client, threads=COMMANDS)
                serial = self._round(client, threads=1)
                parallel = self._round(client, threads=COMMANDS)

        speedup = serial / parallel
        print(f"{COMMANDS} commands, {cores} cores: {serial:.3f}s serial, "
              f"{parallel:.3f}s over {COMMANDS} threads ({speedup:.2f}x)")

        # Half of ideal, which leaves room for scheduling and for the engine's own shared work
        # while still failing anything that merely trends in the right direction. A binding that
        # serialises scores about 1.00x here, which is what this existed to catch.
        floor = COMMANDS / 2
        self.assertGreaterEqual(
            speedup,
            floor,
            f"{COMMANDS} commands over {COMMANDS} threads ran {speedup:.2f}x faster than one "
            f"after another, short of the {floor:.2f}x threads that really run in parallel "
            f"would give: {parallel:.3f}s against {serial:.3f}s",
        )

    def _round(self, client, threads):
        """Runs [`COMMANDS`] aggregations over `threads` threads, and answers how long they took.

        The one-thread round goes through the same executor as the parallel one, so the two
        differ in their thread count and in nothing else.
        """
        database = client.concurrency
        started = time.perf_counter()
        with ThreadPoolExecutor(max_workers=threads) as pool:
            groups = list(pool.map(lambda index: _crunch(database, index), range(COMMANDS)))
        elapsed = time.perf_counter() - started

        self.assertEqual(
            [1] * COMMANDS, groups, "every aggregation should answer exactly one group"
        )
        return elapsed


def _seed(client, collections):
    """Fills `collections` of them, one per command that will be run."""
    for index in range(collections):
        client.concurrency[_collection(index)].insert_many(
            [{"n": n} for n in range(DOCUMENTS)]
        )


def _crunch(database, index, steps=STEPS):
    """One command's work, and how many groups it answered.

    The time is spent with the GIL released rather than in BSON handling, which is what makes
    the wall clock here a measurement of the engine and not of the interpreter.
    """
    return len(list(database[_collection(index)].aggregate(crunch(steps))))


if __name__ == "__main__":
    unittest.main()

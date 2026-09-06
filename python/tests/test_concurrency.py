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
import threading
import time
import unittest
from concurrent.futures import ThreadPoolExecutor

from support import scratch

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

# Terms for the one command the close test overlaps. That command is not being timed, only
# outlasted, so it is sized in seconds against the fraction of one below.
SLOW_STEPS = 6000

# How long the close test lets the command it overlaps get under way. An order of magnitude
# under what `SLOW_STEPS` costs, so the close lands inside the command and not before it.
LEAD = 0.2

# Long enough that only a command that will never finish trips it, short enough that a wedged
# one is reported rather than hanging the suite.
JOIN_SECONDS = 120


class ThreadConcurrencyTest(unittest.TestCase):
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

    def test_closing_waits_for_a_command_without_holding_the_interpreter(self):
        """A close issued while another thread's command runs waits for it, and waits with the
        interpreter released.

        The waiting is not new -- a lock taken exclusively made a close queue behind a running
        command too. What the write guard changes is how long that can be, because it waits out
        every command in flight rather than one, and a close holding the GIL across that would
        stop every unrelated Python thread for the length of somebody else's query. So the
        ticker below counts what an unrelated thread got done while the close was waiting, and
        compares it against what the same thread manages when nothing is competing for the
        interpreter at all.
        """
        answered = {}
        finished = {}
        ticker = _Ticker()

        with scratch() as directory:
            client = MongoClient(f"mongodb_embedded://{directory}", maxPoolSize=COMMANDS)
            try:
                _seed(client, 1)
                running = threading.Event()

                def crunch():
                    running.set()
                    answered["groups"] = _crunch(client.concurrency, 0, steps=SLOW_STEPS)
                    finished["at"] = time.perf_counter()

                ticker.start()
                worker = threading.Thread(target=crunch)
                worker.start()
                running.wait()

                # The reference rate, measured while the command is under way and this thread
                # is asleep: nothing holds the interpreter, so the ticker has it to itself.
                # Doubles as the head start that puts the close inside the command rather than
                # before it.
                free = ticker.rate(lambda: time.sleep(LEAD))
                closing_at = time.perf_counter()
                held = ticker.rate(client.close)
                closed_at = time.perf_counter()

                worker.join(timeout=JOIN_SECONDS)
            finally:
                # Before any assertion: a ticker left running spends the rest of the process
                # taking the interpreter away from the other test in this file, which would
                # turn one failure into two.
                ticker.stop()
            # A caller that closes explicitly inside a `with` block closes twice.
            client.close()

        self.assertFalse(
            worker.is_alive(),
            f"the command was still running {JOIN_SECONDS}s after the close returned",
        )
        self.assertEqual(
            {"groups": 1},
            answered,
            "the command a close overlapped should still have answered its one group",
        )
        self.assertGreater(
            finished["at"],
            closing_at,
            "the command finished before the close was issued, so this measured a close that "
            "overlapped nothing -- SLOW_STEPS is too small for this machine",
        )
        # The other end of the same overlap, and the half the name of this test claims: a close
        # that gave up rather than waiting -- `try_write` and a shrug -- would return in
        # microseconds, well ahead of the command it was supposed to wait out.
        self.assertGreater(
            closed_at,
            finished["at"],
            "the close returned before the command it overlapped finished, so it did not wait "
            "for it",
        )
        # Half the free rate, which leaves room for the answer the command hands back through
        # the interpreter on its way out. A close that waited with the GIL held scores near
        # zero instead: no bytecode runs at all while it blocks.
        self.assertGreater(
            held,
            free / 2,
            f"an unrelated thread ran at {held:,.0f} ticks/s while the close waited, against "
            f"{free:,.0f} with the interpreter free -- the close is holding the GIL across a "
            f"wait as long as another thread's command",
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


class _Ticker:
    """A thread that does nothing but count, so a stretch in which no other Python thread could
    run shows up as a count that barely moved.

    Rates rather than totals, and every rate measured against another taken on the same
    machine: how fast a counting loop goes is a property of the interpreter and the hardware,
    and only the ratio between two of them says anything about the GIL.
    """

    def __init__(self):
        self._stop = threading.Event()
        self._thread = threading.Thread(target=self._count, daemon=True)
        self.ticks = 0

    def start(self):
        self._thread.start()

    def rate(self, action):
        """Runs `action` and answers how many ticks a second the counter managed meanwhile."""
        before = self.ticks
        started = time.perf_counter()
        action()
        elapsed = time.perf_counter() - started
        return (self.ticks - before) / elapsed

    def stop(self):
        """Stops the counter, and is safe to call whether or not it was ever started."""
        self._stop.set()
        if self._thread.ident is not None:
            self._thread.join(timeout=JOIN_SECONDS)

    def _count(self):
        while not self._stop.is_set():
            self.ticks += 1


def _seed(client, collections):
    """Fills `collections` of them, one per command that will be run."""
    for index in range(collections):
        client.concurrency[_collection(index)].insert_many(
            [{"n": n} for n in range(DOCUMENTS)]
        )


def _crunch(database, index, steps=STEPS):
    """One command's work, and how many groups it answered.

    Arithmetic the engine has to compute rather than fetch, so the clock measures execution and
    not I/O, and so the time is spent with the GIL released rather than in BSON handling. One
    collection per index, so two commands contend only where two MongoDB connections would.
    """
    pipeline = [
        {
            "$project": {
                "crunched": {
                    "$reduce": {
                        "input": {"$range": [0, steps]},
                        "initialValue": 0,
                        "in": {"$add": ["$$value", {"$multiply": ["$$this", "$$this"]}]},
                    }
                }
            }
        },
        {"$group": {"_id": None, "total": {"$sum": "$crunched"}}},
    ]
    return len(list(database[_collection(index)].aggregate(pipeline)))


def _collection(index):
    return f"numbers_{index}"


if __name__ == "__main__":
    unittest.main()

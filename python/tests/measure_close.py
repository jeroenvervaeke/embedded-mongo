"""That closing waits for the commands other threads are running, and waits off the GIL.

A close is the one caller that excludes commands rather than joining them, so it is the one
place where the read guard every command holds has to be waited out. Two things can go wrong
there and this measures both: cutting a running command off, and waiting for it while holding
the interpreter, which would stop every unrelated Python thread for the length of somebody
else's query.
"""

import threading
import time
import unittest

from support import collection_name, crunch, scratch

from pymongo_embedded import MongoClient

# Connections the client may hand out. Only one command is in flight here, but the close test
# and the concurrency test share a shape, and a pool of one would hide a mistake in either.
CONNECTIONS = 8

# Documents in the one collection this seeds.
DOCUMENTS = 1500

# Terms summed per document. Sized in seconds rather than milliseconds: the command is not
# being timed here, only outlasted by the head start below.
SLOW_STEPS = 6000

# How long the close is given to land inside the command rather than before it. An order of
# magnitude under what SLOW_STEPS costs.
LEAD = 0.2

# Long enough that only a command that will never finish trips it, short enough that a wedged
# one is reported rather than hanging the suite.
JOIN_SECONDS = 120


class CloseMeasurement(unittest.TestCase):
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
            client = MongoClient(f"mongodb_embedded://{directory}", maxPoolSize=CONNECTIONS)
            try:
                _seed(client)
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


def _seed(client):
    client.concurrency[collection_name(0)].insert_many(
        [{"n": n} for n in range(DOCUMENTS)]
    )


def _crunch(database, index, steps):
    """One command's work, and how many groups it answered."""
    return len(list(database[collection_name(index)].aggregate(crunch(steps))))


if __name__ == "__main__":
    unittest.main()

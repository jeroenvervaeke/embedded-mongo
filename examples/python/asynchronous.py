"""The embedded engine from asyncio, and the two things that are only true from here.

Run with:

    ./scripts/python examples/python/asynchronous.py
"""

import asyncio
import time
from pathlib import Path

from pymongo_embedded import AsyncMongoClient

DOCUMENTS = 2000

# Arithmetic the engine has to compute rather than fetch, so a command takes long enough to
# watch. Each command reads its own collection, as two MongoDB connections would.
CRUNCH = [
    {"$project": {"crunched": {"$reduce": {
        "input": {"$range": [0, 4000]},
        "initialValue": 0,
        "in": {"$add": ["$$value", {"$multiply": ["$$this", "$$this"]}]},
    }}}},
    {"$group": {"_id": None, "total": {"$sum": "$crunched"}}},
]


# Beside the repository rather than in the system temporary directory, which on a good many
# Linux machines is a memory filesystem -- and the engine preallocates a couple of hundred
# megabytes of journal for every directory it opens.
DATA_DIR = Path(__file__).resolve().parents[2] / ".cache/python-async-example-data"


async def main():
    # Blocking, on purpose: opening does storage recovery, which is not work for an event loop.
    client = AsyncMongoClient(f"mongodb_embedded://{DATA_DIR}")
    try:
        # Runnable twice: the example asserts on counts, so it starts from a known state.
        await client.drop_database("app")
        await ordinary_commands(client)
        await commands_in_parallel(client)
        await a_timeout_that_stops_the_engine(client)
    finally:
        await client.close()


async def ordinary_commands(client):
    items = client.app.items
    await items.insert_many([{"_id": n, "value": n * 2} for n in range(5)])
    await items.update_one({"_id": 1}, {"$inc": {"value": 10}})
    values = [item["value"] async for item in items.find().sort("_id")]
    print(f"values: {values}")
    print(f"documents: {await items.count_documents({})}")


async def commands_in_parallel(client):
    """Eight commands at once cost about what one does: the engine runs them on eight sessions,
    and awaiting them costs the loop eight parked tasks."""
    for index in range(8):
        await client.app[f"numbers_{index}"].insert_many(
            [{"n": n} for n in range(DOCUMENTS)]
        )

    started = time.perf_counter()
    await crunch(client, 0)
    one = time.perf_counter() - started

    started = time.perf_counter()
    await asyncio.gather(*(crunch(client, index) for index in range(8)))
    eight = time.perf_counter() - started

    print(f"one command {one:.3f}s; eight gathered {eight:.3f}s ({8 * one / eight:.1f}x)")


async def a_timeout_that_stops_the_engine(client):
    """A timeout here bounds the work, not just the wait.

    Dropping the future interrupts the command in the engine and returns its session, so the
    ping afterwards runs at once instead of queueing behind a query nobody is waiting for.
    """
    started = time.perf_counter()
    try:
        await asyncio.wait_for(crunch(client, 0), timeout=0.1)
    except TimeoutError:
        print(f"gave up after {time.perf_counter() - started:.3f}s")

    started = time.perf_counter()
    await client.admin.command("ping")
    print(f"ping straight afterwards: {time.perf_counter() - started:.3f}s")


async def crunch(client, index):
    cursor = await client.app[f"numbers_{index}"].aggregate(CRUNCH)
    return await cursor.to_list()


asyncio.run(main())

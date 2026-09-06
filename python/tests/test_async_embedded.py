"""The async client answers the same URIs and runs the same commands as the synchronous one.

Parity rather than novelty: everything here has a counterpart in test_embedded.py, and the
point of running it twice is that the two surfaces share a URI parser and a handshake but not
a pool, a connection or a client -- so only exercising both proves the async half is wired to
the same engine.
"""

import unittest

from pymongo.errors import DuplicateKeyError
from support import scratch

from pymongo_embedded import AsyncMongoClient, MongoClient


class AsyncEmbeddedMongoClientTest(unittest.IsolatedAsyncioTestCase):
    async def test_regular_and_embedded_clients(self):
        remote = AsyncMongoClient("mongodb://localhost:27017/", connect=False)
        self.assertIsNone(remote._embedded_runtime)
        await remote.close()

        with scratch() as directory:
            local = AsyncMongoClient(f"mongodb_embedded://{directory}")
            try:
                items = local.test.items
                self.assertEqual(1.0, (await local.admin.command("ping"))["ok"])
                await items.insert_many([{"_id": 1, "value": 2}, {"_id": 2, "value": 1}])
                self.assertEqual(
                    [1, 2],
                    [item["value"] async for item in items.find().sort("value")],
                )
                await items.update_one({"_id": 1}, {"$inc": {"value": 3}})
                cursor = await items.aggregate(
                    [{"$group": {"_id": None, "n": {"$sum": "$value"}}}]
                )
                self.assertEqual(6, (await cursor.next())["n"])
                with self.assertRaises(DuplicateKeyError):
                    await items.insert_one({"_id": 1})
                await items.delete_one({"_id": 2})
                self.assertEqual(1, await items.count_documents({}))
            finally:
                await local.close()

    async def test_a_cursor_spanning_batches_is_drained(self):
        """More documents than one reply carries, so `getMore` is exercised.

        Each batch is its own command and so its own await, which is the part of the loop this
        binding is unusual in: a cursor is where an embedded client issues commands it was
        never explicitly asked for.
        """
        with scratch() as directory:
            local = AsyncMongoClient(f"mongodb_embedded://{directory}")
            try:
                await local.test.many.insert_many([{"n": n} for n in range(1000)])
                seen = [document["n"] async for document in local.test.many.find().sort("n")]
                self.assertEqual(list(range(1000)), seen)
            finally:
                await local.close()

    async def test_closing_releases_the_process_wide_runtime(self):
        """One engine per process, and closing gives it back.

        Without this the async client would be a one-shot: an application that opened one, closed
        it and opened another would be refused by the engine rather than by anything here.
        """
        with scratch() as directory:
            first = AsyncMongoClient(f"mongodb_embedded://{directory}")
            await first.test.items.insert_one({"_id": 1})
            await first.close()

            second = AsyncMongoClient(f"mongodb_embedded://{directory}")
            try:
                self.assertEqual(1, await second.test.items.count_documents({}))
            finally:
                await second.close()

    async def test_a_second_engine_is_refused_legibly(self):
        """A synchronous and an asynchronous client cannot both be open.

        The engine is a process-wide singleton, so this is a real constraint rather than a
        binding's choice, and the only thing worth testing is that it arrives as a sentence
        somebody can act on rather than as a crash or an obscure code.
        """
        with scratch() as directory:
            local = AsyncMongoClient(f"mongodb_embedded://{directory}")
            try:
                with self.assertRaises(RuntimeError) as refused:
                    MongoClient(f"mongodb_embedded://{directory}/second")
                self.assertIn(
                    "only one embedded MongoDB runtime may be open per process",
                    str(refused.exception),
                )
                # Refusing must not have cost the first client anything.
                self.assertEqual(1.0, (await local.admin.command("ping"))["ok"])
            finally:
                await local.close()


if __name__ == "__main__":
    unittest.main()

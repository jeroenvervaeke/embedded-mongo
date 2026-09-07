"""The async client answers the same URIs and runs the same commands as the synchronous one.

The two surfaces share a URI parser and a handshake and nothing else -- not a pool, not a
connection, not a client -- so the CRUD here is deliberately the same as test_embedded.py's,
and running it twice is what proves the async half is wired to the same engine.

The rest is particular to this client: what a cursor does across batches when every `getMore`
is its own await, what closing gives back, and what happens to a constructor that opened an
engine and then failed.
"""

import unittest
from unittest import mock

from pymongo.errors import AutoReconnect, DuplicateKeyError, InvalidOperation
from support import scratch

from pymongo_embedded import AsyncMongoClient, MongoClient
from pymongo_embedded.asynchronous.pool import AsyncEmbeddedConnection, AsyncEmbeddedPool


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

    async def test_a_constructor_that_fails_closes_the_engine_it_opened(self):
        """The engine is opened before PyMongo is, so a constructor that then fails is holding
        the process's one runtime with nothing left to reach it.

        Nothing else can open an engine until that is given back, which makes this the failure
        that costs the most and shows the least: the traceback names a bad option, and every
        later client is refused for a reason that has nothing to do with it.
        """
        with scratch() as directory:
            with self.assertRaises(Exception) as failed:
                AsyncMongoClient(f"mongodb_embedded://{directory}", maxPoolSize=-1)
            self.assertNotIsInstance(failed.exception, RuntimeError)

            # The proof: this is refused if the failed constructor kept the runtime.
            after = AsyncMongoClient(f"mongodb_embedded://{directory}")
            try:
                self.assertEqual(1.0, (await after.admin.command("ping"))["ok"])
            finally:
                await after.close()

    async def test_a_port_or_a_pool_class_is_refused(self):
        """Both are meaningless against a directory, and both would otherwise be ignored -- a
        port silently, a pool class by quietly replacing the one thing that reaches the engine.
        """
        with scratch() as directory:
            uri = f"mongodb_embedded://{directory}"
            with self.assertRaises(TypeError):
                AsyncMongoClient(uri, 27017)
            with self.assertRaises(TypeError):
                AsyncMongoClient(uri, _pool_class=object)

    async def test_a_closed_client_refuses_further_commands(self):
        with scratch() as directory:
            client = AsyncMongoClient(f"mongodb_embedded://{directory}")
            await client.close()
            with self.assertRaises(InvalidOperation):
                await client.admin.command("ping")
            # Closing twice is what a `try`/`finally` around an explicit close does.
            await client.close()

    async def test_a_failed_handshake_goes_through_pymongos_own_error_handling(self):
        """A connection that fails its handshake goes through the base class's own error handling.

        There is no realistic way for this binding's handshake to fail -- it is a local
        function returning a constant, with no socket to refuse it -- so what is worth
        pinning is not the scenario but the wiring. `connect` is an override of PyMongo's,
        and an override that quietly dropped a step of the contract would leave embedded
        pools reporting failures differently from every other pool, with nothing to say so.
        """
        async def refuse(self):
            raise AutoReconnect("handshake refused")

        with scratch() as directory:
            client = AsyncMongoClient(f"mongodb_embedded://{directory}")
            try:
                # Only `hello`, which is what `connect` calls. The monitor reaches for `_hello`
                # and so keeps working, which is what lets the error reach this caller intact
                # instead of being absorbed into a server-selection timeout.
                with (
                    mock.patch.object(AsyncEmbeddedConnection, "hello", refuse),
                    mock.patch.object(
                        AsyncEmbeddedPool,
                        "_handle_connection_error",
                        side_effect=AsyncEmbeddedPool._handle_connection_error,
                        autospec=True,
                    ) as handled,
                ):
                    with self.assertRaises(AutoReconnect):
                        await client.admin.command("ping")
                handled.assert_called_once()
                self.assertTrue(
                    handled.call_args.args[1].has_error_label("SystemOverloadedError"),
                    "the handshake failure reached the base class but came back unlabelled",
                )
            finally:
                await client.close()


if __name__ == "__main__":
    unittest.main()

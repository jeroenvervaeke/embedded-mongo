import gzip
import os
import shutil
import tempfile
import unittest
from pathlib import Path

from pymongo.errors import DuplicateKeyError

from pymongo_embedded import MongoClient

_REPO_ROOT = Path(__file__).resolve().parents[2]

# The committed remains of a directory the pre-fix engine damaged: three documents written
# after a reopen went into the record store and into no index at all, one of them a second copy
# of `_id` 1 that an unmaintained `_id_` accepted. See tests/repair/fixture.rs for how it was
# made and what is in it.
_DAMAGED_FIXTURE = _REPO_ROOT / "tests" / "fixtures" / "damaged-reopen"

# How many files the fixture is made of, asserted on unpacking so a half-committed fixture
# fails as a fixture problem rather than as an unexplained engine error.
_FIXTURE_FILES = 15

# The file the one-time index repair pass writes once it has visited every collection.
# Spelled out here because Python cannot reach the Rust constant; src/repair/marker.rs owns it.
_MARKER = ".embedded-mongodb-index-repair"

# The switch that suppresses the pass, named only so a failure can blame it.
_SKIP_VARIABLE = "EMBEDDED_MONGODB_SKIP_INDEX_REPAIR"


def _skip_hint():
    """Names the skip switch when the environment sets it.

    A suppressed pass and a broken one leave a directory looking exactly alike, so without
    this a stray variable in the shell reads as a regression. Appended to the message rather
    than asserted up front, so that setting the variable stays a usable way to check that
    these assertions really do detect a pass that never ran.
    """
    value = os.environ.get(_SKIP_VARIABLE)
    if not value:
        return ""
    return f" ({_SKIP_VARIABLE}={value} is set in the environment, which suppresses the pass)"


def _scratch():
    """A temporary directory under `target`, never the system one.

    /tmp is a memory filesystem on a good many Linux machines, and the engine preallocates a
    couple of hundred megabytes of WiredTiger journal for every data directory it opens,
    however few documents go in.
    """
    base = _REPO_ROOT / "target" / "tmp"
    base.mkdir(parents=True, exist_ok=True)
    return tempfile.TemporaryDirectory(dir=base)


def _unpack_damaged(path):
    """Unpacks the damaged directory into `path`, which must not exist yet."""
    path.mkdir(parents=True)
    unpacked = 0
    for source in _DAMAGED_FIXTURE.iterdir():
        if source.suffix != ".gz":
            continue
        with gzip.open(source, "rb") as compressed:
            with open(path / source.stem, "wb") as target:
                shutil.copyfileobj(compressed, target)
        unpacked += 1
    if unpacked != _FIXTURE_FILES:
        raise AssertionError(
            f"unpacked {unpacked} files from {_DAMAGED_FIXTURE}, expected {_FIXTURE_FILES}"
        )


class EmbeddedMongoClientTest(unittest.TestCase):
    def test_regular_and_embedded_clients(self):
        remote = MongoClient("mongodb://localhost:27017/", connect=False)
        self.assertIsNone(remote._embedded_runtime)
        remote.close()

        with _scratch() as directory:
            with MongoClient(f"mongodb_embedded://{directory}") as local:
                items = local.test.items
                self.assertEqual(1.0, local.admin.command("ping")["ok"])
                items.insert_many([{"_id": 1, "value": 2}, {"_id": 2, "value": 1}])
                self.assertEqual(
                    [1, 2],
                    [item["value"] for item in items.find().sort("value")],
                )
                items.update_one({"_id": 1}, {"$inc": {"value": 3}})
                total = next(items.aggregate([{"$group": {"_id": None, "n": {"$sum": "$value"}}}]))
                self.assertEqual(6, total["n"])
                with self.assertRaises(DuplicateKeyError):
                    items.insert_one({"_id": 1})
                items.delete_one({"_id": 2})
                self.assertEqual(1, items.count_documents({}))

    def test_repairs_a_directory_an_older_build_damaged(self):
        """Opening goes through `embedded_mongodb::Client`, so the one-time index repair pass
        runs here too.

        It used to open the raw FFI client, which skips the pass, and a Python application
        pointed at a directory some earlier build wrote is one of the two consumers likeliest
        to hold that damage. Every reading below is one that changes when the pass is skipped.
        """
        with _scratch() as scratch:
            damaged = Path(scratch) / "damaged"
            _unpack_damaged(damaged)

            with MongoClient(f"mongodb_embedded://{damaged}") as local:
                self.assertTrue(
                    (damaged / _MARKER).is_file(),
                    "open left no marker, so the repair pass never ran" + _skip_hint(),
                )
                shop = local.shop

                # 0 without the pass: `customer` c5 was written after the reopen and never
                # reached `customer_1`, so an indexed lookup skipped it while a collection
                # scan still returned it.
                self.assertEqual(1, shop.command("count", "orders", query={"customer": "c5"})["n"])
                self.assertEqual(1, shop.command("count", "orders", query={"_id": 5})["n"])
                # A second database, so the pass is seen to have crossed a database boundary.
                self.assertEqual(1, local.catalog.command("count", "items", query={"_id": 3})["n"])

                # Six, not the seven the record store held: the seventh was the second copy of
                # `_id` 1, which the repair moved rather than deleted.
                self.assertEqual(6, shop.command("count", "orders", query={})["n"])
                self.assertEqual(1, shop.command("count", "orders", query={"_id": 1})["n"])
                self.assertEqual(1, shop.command("count", "untouched", query={})["n"])

                moved = [
                    name
                    for name in local.local.list_collection_names()
                    if name.startswith("lost_and_found.")
                ]
                # The counts above cannot tell a document that was moved from one that was
                # deleted, and `validate {repair: true}` can do both. This is the difference.
                self.assertEqual(
                    1,
                    len(moved),
                    f"expected one lost and found, found {moved}" + _skip_hint(),
                )
                # Neither copy of the duplicate `_id` was destroyed: one is still in the
                # collection and the other is in the lost and found.
                self.assertEqual(
                    {"c1", "duplicate"},
                    {shop.orders.find_one({"_id": 1})["customer"]}
                    | {document["customer"] for document in local.local[moved[0]].find({})},
                )

                # The `_id_` index is maintained again, which is what accepting the duplicate
                # in the first place proved it was not.
                with self.assertRaises(DuplicateKeyError):
                    shop.orders.insert_one({"_id": 1, "customer": "another duplicate"})


if __name__ == "__main__":
    unittest.main()

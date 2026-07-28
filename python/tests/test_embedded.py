import tempfile
import unittest

from pymongo.errors import DuplicateKeyError

from pymongo_embedded import MongoClient


class EmbeddedMongoClientTest(unittest.TestCase):
    def test_regular_and_embedded_clients(self):
        remote = MongoClient("mongodb://localhost:27017/", connect=False)
        self.assertIsNone(remote._embedded_runtime)
        remote.close()

        with tempfile.TemporaryDirectory() as directory:
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


if __name__ == "__main__":
    unittest.main()

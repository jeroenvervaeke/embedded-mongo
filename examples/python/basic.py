from pathlib import Path

from pymongo_embedded import MongoClient


data_dir = Path(__file__).resolve().parents[2] / ".cache/python-example-data"

with MongoClient(f"mongodb_embedded://{data_dir}") as client:
    items = client.demo.items
    items.update_one(
        {"_id": "hello"},
        {
            "$inc": {"runs": 1},
            "$set": {"message": "MongoDB is running inside Python"},
        },
        upsert=True,
    )
    item = items.find_one({"_id": "hello"})
    print(f"Run #{item['runs']}: {item['message']}")

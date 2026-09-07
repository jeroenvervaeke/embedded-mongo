"""What more than one test file here needs."""

import tempfile
from pathlib import Path

_REPO_ROOT = Path(__file__).resolve().parents[2]


def scratch():
    """A temporary directory under `target`, never the system one.

    /tmp is a memory filesystem on a good many Linux machines, and the engine preallocates a
    couple of hundred megabytes of WiredTiger journal for every data directory it opens,
    however few documents go in.
    """
    base = _REPO_ROOT / "target" / "tmp"
    base.mkdir(parents=True, exist_ok=True)
    return tempfile.TemporaryDirectory(dir=base)


def crunch(steps):
    """A pipeline whose cost is arithmetic the engine has to compute rather than fetch.

    `steps` terms are summed per document, so the work scales with it and with how many
    documents the collection holds. Shared because the synchronous and asynchronous concurrency
    tests must measure the same command to be comparable at all.
    """
    return [
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


def collection_name(index):
    """One collection per command, so two running at once contend only where two MongoDB
    connections would."""
    return f"numbers_{index}"

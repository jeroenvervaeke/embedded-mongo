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

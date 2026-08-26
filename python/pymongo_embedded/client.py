from __future__ import annotations

from functools import partial
from typing import TYPE_CHECKING, Any
from urllib.parse import unquote

import pymongo
from pymongo import MongoClient as _MongoClient

if TYPE_CHECKING:
    from ._native import NativeClient

_SCHEMES = ("mongodb+embedded://", "mongodb_embedded://")

# 4.17 is the newest PyMongo that exists. The binding was written against a source
# checkout of mongo-python-driver, whose in-development version reports 4.18, and pinning
# to that made the package uninstallable: pip cannot resolve a release that was never cut.
if pymongo.version_tuple[:2] != (4, 17):
    raise ImportError("pymongo-embedded 0.1 requires PyMongo 4.17.x")


def _path(uri: object) -> str | None:
    if not isinstance(uri, str):
        return None
    for scheme in _SCHEMES:
        if uri.startswith(scheme):
            path = uri[len(scheme) :]
            if not path or "?" in path or "#" in path:
                raise ValueError("embedded MongoDB URI must contain only a database directory")
            return unquote(path)
    return None


class MongoClient(_MongoClient):
    def __init__(
        self,
        host: Any = None,
        port: int | None = None,
        document_class: type | None = None,
        tz_aware: bool | None = None,
        connect: bool | None = None,
        type_registry: Any = None,
        **kwargs: Any,
    ) -> None:
        self._embedded_runtime: NativeClient | None = None
        path = _path(host)
        if path is None:
            super().__init__(
                host, port, document_class, tz_aware, connect, type_registry, **kwargs
            )
            return
        if port is not None:
            raise TypeError("port is not valid with an embedded MongoDB URI")
        if "_pool_class" in kwargs:
            raise TypeError("_pool_class cannot be combined with embedded MongoDB")

        from ._native import NativeClient
        from .pool import EmbeddedPool

        runtime = NativeClient(path)
        self._embedded_runtime = runtime
        kwargs["_pool_class"] = partial(EmbeddedPool, runtime=runtime)
        kwargs.setdefault("directConnection", True)
        kwargs.setdefault("retryReads", False)
        kwargs.setdefault("retryWrites", False)
        try:
            super().__init__(
                "embedded",
                27017,
                document_class,
                tz_aware,
                connect,
                type_registry,
                **kwargs,
            )
        except BaseException:
            self._embedded_runtime = None
            runtime.close()
            raise

    def close(self) -> None:
        runtime = self._embedded_runtime
        try:
            super().close()
        finally:
            if runtime is not None:
                self._embedded_runtime = None
                runtime.close()

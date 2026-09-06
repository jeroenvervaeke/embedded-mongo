from __future__ import annotations

from functools import partial
from typing import TYPE_CHECKING, Any

from pymongo import AsyncMongoClient as _AsyncMongoClient

from ..common import path_from_uri

if TYPE_CHECKING:
    from .._native import AsyncNativeClient


class AsyncMongoClient(_AsyncMongoClient):
    """PyMongo's async client, with embedded URIs routed to the in-process engine.

    The same substitution the synchronous
    [`MongoClient`](pymongo_embedded.client.MongoClient) makes, over PyMongo's asynchronous
    classes: an address this does not recognise is handed to PyMongo untouched, so one class
    serves both a real server and a directory on disk.

    Two things it has that the synchronous client cannot. The event loop is never blocked for
    the length of a command -- the engine's own worker threads do the blocking, and awaiting
    costs a parked task. And a cancelled task really cancels: `asyncio.wait_for` around an
    operation stops the engine working rather than only stopping the wait, and hands the
    session straight back.

    Only one embedded engine may be open per process, synchronous or asynchronous. Opening a
    second says so.
    """

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
        self._embedded_runtime: AsyncNativeClient | None = None
        path = path_from_uri(host)
        if path is None:
            super().__init__(
                host, port, document_class, tz_aware, connect, type_registry, **kwargs
            )
            return
        if port is not None:
            raise TypeError("port is not valid with an embedded MongoDB URI")
        if "_pool_class" in kwargs:
            raise TypeError("_pool_class cannot be combined with embedded MongoDB")

        from .._native import AsyncNativeClient
        from .pool import AsyncEmbeddedPool

        # Synchronously, before the loop is ever handed back: opening does storage recovery and
        # the one-time index repair scan, and there is no version of that an event loop should
        # be running. It is also what makes this a constructor rather than something to await,
        # which is what PyMongo's API says it is.
        runtime = AsyncNativeClient(path)
        self._embedded_runtime = runtime
        kwargs["_pool_class"] = partial(AsyncEmbeddedPool, runtime=runtime)
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
            # The blocking close, because this is a constructor: there is no loop to await on,
            # and leaving the engine open would cost the whole process its one runtime.
            self._embedded_runtime = None
            runtime.close_blocking()
            raise

    async def close(self) -> None:
        runtime = self._embedded_runtime
        try:
            await super().close()
        finally:
            if runtime is not None:
                self._embedded_runtime = None
                await runtime.close()

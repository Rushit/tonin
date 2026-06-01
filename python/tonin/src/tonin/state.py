"""Pre-wired async DB + cache connections.

Mirror of the Rust ``tonin_core::state`` module. ``State`` holds the
async connection handles a Python service needs; constructed at boot
via ``State.from_env()`` and threaded through to handlers.

All operations are async — there is no sync escape hatch. The Python
ecosystem has too many ways to accidentally block the event loop
(``psycopg2.connect``, ``requests.get``, ``redis.Redis()``); the
framework prevents that by only offering async clients:

- Postgres: ``asyncpg.Pool`` (not psycopg2/psycopg)
- Redis: ``redis.asyncio.Redis`` (not the sync ``redis.Redis``)

Activation matches Rust exactly:

==================== ============================================
Env var              What it produces
==================== ============================================
``DATABASE_URL``     :class:`asyncpg.Pool`
``REDIS_URL``        :class:`redis.asyncio.Redis`
==================== ============================================

Absent env → field stays ``None``, no connection attempt. Set-but-
unreachable → ``State.from_env()`` raises at startup (fail-fast).

Pool tuning: ``TONIN_PG_MAX_CONNECTIONS`` (default ``10``).
"""

from __future__ import annotations

import logging
import os
from dataclasses import dataclass
from typing import Protocol

logger = logging.getLogger(__name__)


class StorageProvider(Protocol):
    """Pluggable object-storage backend.

    The scaffold's default impl (emitted by ``--with-storage``) wraps
    an :class:`opendal.AsyncOperator`, but anything matching this
    protocol works. The framework only calls :meth:`probe` at boot;
    every other call goes through whatever surface the impl exposes.

    To swap providers, write your own class with ``async def probe``
    and an optional ``def system``, then call
    ``await state.with_storage(MyProvider(...))`` from main.
    """

    async def probe(self) -> None:
        """Cheap connectivity check. Raises if storage is unreachable."""
        ...


@dataclass(slots=True)
class State:
    """Bundle of optional async connection handles.

    Cheap to pass around — all inner types are reference-counted by
    their respective libraries. Don't construct directly; use
    :meth:`State.from_env` (then optionally :meth:`with_storage`).
    """

    pg: object | None = None  # asyncpg.Pool — typed as object to avoid hard import
    redis: object | None = None  # redis.asyncio.Redis
    storage: object | None = None  # StorageProvider — opt-in via --with-storage

    @classmethod
    async def from_env(cls) -> "State":
        """Build a ``State`` from environment variables.

        Tries each backend independently; missing env produces ``None``
        fields, not errors. Connection failures DO error: misconfigured
        deps should fail fast at startup rather than at handler time.

        Object storage is **not** initialized here — its concrete client
        type is scaffold-time opt-in. The scaffolded ``main.py`` calls
        :meth:`with_storage` separately when ``--with-storage`` was used.
        """
        pg = await _maybe_pg_pool()
        redis_client = await _maybe_redis_client()
        return cls(pg=pg, redis=redis_client)

    async def with_storage(self, storage: object) -> "State":
        """Attach a storage provider. Runs its probe immediately and
        raises on failure (same fail-fast-at-boot posture as DB/Redis).
        Returns ``self`` for chaining.
        """
        probe = getattr(storage, "probe", None)
        if probe is None:
            raise RuntimeError(
                "storage provider must expose `async def probe`"
            )
        await probe()
        self.storage = storage
        return self

    def has_pg(self) -> bool:
        return self.pg is not None

    def has_redis(self) -> bool:
        return self.redis is not None

    def has_storage(self) -> bool:
        return self.storage is not None

    def require_pg(self) -> object:
        """Return the asyncpg Pool. Raises if ``DATABASE_URL`` was unset."""
        if self.pg is None:
            raise RuntimeError(
                "postgres requested but DATABASE_URL was not set at startup"
            )
        return self.pg

    def require_redis(self) -> object:
        """Return the redis.asyncio.Redis client. Raises if ``REDIS_URL`` was unset."""
        if self.redis is None:
            raise RuntimeError(
                "redis requested but REDIS_URL was not set at startup"
            )
        return self.redis

    def require_storage(self) -> object:
        """Return the storage provider. Raises if nothing was wired."""
        if self.storage is None:
            raise RuntimeError(
                "storage requested but no provider was wired at startup "
                "(scaffold with --with-storage to enable)"
            )
        return self.storage

    async def close(self) -> None:
        """Close pools cleanly. Call from the service's shutdown path."""
        if self.pg is not None:
            close = getattr(self.pg, "close", None)
            if close is not None:
                await close()
        if self.redis is not None:
            close = getattr(self.redis, "aclose", None)
            if close is not None:
                await close()
        if self.storage is not None:
            close = getattr(self.storage, "close", None)
            if close is not None:
                await close()


async def _maybe_pg_pool() -> object | None:
    url = os.environ.get("DATABASE_URL")
    if not url:
        return None
    try:
        import asyncpg
    except ImportError as e:
        raise RuntimeError(f"DATABASE_URL set but asyncpg not installed: {e}") from e

    max_size = int(os.environ.get("TONIN_PG_MAX_CONNECTIONS", "10"))
    logger.info("connecting to postgres (max_size=%d)", max_size)
    try:
        pool = await asyncpg.create_pool(url, max_size=max_size)
    except Exception as e:  # noqa: BLE001
        raise RuntimeError(f"postgres connect failed: {e}") from e
    return pool


async def _maybe_redis_client() -> object | None:
    url = os.environ.get("REDIS_URL")
    if not url:
        return None
    try:
        from redis.asyncio import Redis
    except ImportError as e:
        raise RuntimeError(f"REDIS_URL set but redis not installed: {e}") from e

    logger.info("connecting to redis")
    try:
        client = Redis.from_url(url)
        # Eagerly verify reachability so a misconfigured cache fails fast.
        await client.ping()
    except Exception as e:  # noqa: BLE001
        raise RuntimeError(f"redis connect failed: {e}") from e
    return client

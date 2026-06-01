"""Unit tests for tonin.state.

The connection-attempt path requires real DB/Redis, so we only test
the env-driven empty-state path here. Integration coverage with real
containers lands in the scaffold's e2e tests.
"""

from __future__ import annotations

import os

import pytest

from tonin.state import State


@pytest.mark.asyncio
async def test_empty_state_when_no_env_vars(monkeypatch) -> None:
    monkeypatch.delenv("DATABASE_URL", raising=False)
    monkeypatch.delenv("REDIS_URL", raising=False)
    s = await State.from_env()
    assert not s.has_pg()
    assert not s.has_redis()
    assert not s.has_storage()
    with pytest.raises(RuntimeError):
        s.require_pg()
    with pytest.raises(RuntimeError):
        s.require_redis()
    with pytest.raises(RuntimeError):
        s.require_storage()


class _MockStorage:
    """Implements the StorageProvider protocol structurally."""

    def __init__(self, fail: bool = False) -> None:
        self.probes = 0
        self.fail = fail

    async def probe(self) -> None:
        self.probes += 1
        if self.fail:
            raise RuntimeError("mock probe failure")


@pytest.mark.asyncio
async def test_with_storage_runs_probe() -> None:
    s = State()
    storage = _MockStorage()
    s2 = await s.with_storage(storage)
    assert s2 is s  # chaining returns self
    assert s.has_storage()
    assert storage.probes == 1


@pytest.mark.asyncio
async def test_with_storage_propagates_probe_failure() -> None:
    s = State()
    with pytest.raises(RuntimeError, match="mock probe failure"):
        await s.with_storage(_MockStorage(fail=True))
    assert not s.has_storage()


@pytest.mark.asyncio
async def test_with_storage_rejects_missing_probe() -> None:
    s = State()

    class NoProbe:
        pass

    with pytest.raises(RuntimeError, match="async def probe"):
        await s.with_storage(NoProbe())

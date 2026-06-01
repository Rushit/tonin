"""Unit tests for the Service builder.

End-to-end behavior (interceptors actually firing, contextvar flowing
to the handler) is covered by the scaffold's e2e tests with a real
grpc.aio server.
"""

from __future__ import annotations

import pytest

from tonin import Service


def test_default_addr() -> None:
    svc = Service.new("test")
    assert svc._addr == "0.0.0.0:50051"


def test_addr_override() -> None:
    svc = Service.new("test").addr("127.0.0.1:0")
    assert svc._addr == "127.0.0.1:0"


def test_with_auth_installs_interceptor() -> None:
    class DummyVerifier:
        async def verify(self, token):  # type: ignore[no-untyped-def]
            ...

    svc = Service.new("test").with_auth(DummyVerifier())
    assert svc._interceptor is not None
    assert svc._interceptor._optional is False


def test_without_auth_is_optional_anon() -> None:
    svc = Service.new("test").without_auth()
    assert svc._interceptor is not None
    assert svc._interceptor._optional is True


@pytest.mark.asyncio
async def test_run_without_handler_fails() -> None:
    svc = Service.new("test").without_auth().addr("127.0.0.1:0")
    with pytest.raises(RuntimeError, match="no handler registered"):
        await svc.run()

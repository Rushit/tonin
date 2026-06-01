"""tonin: opinionated Python microservice framework.

Sibling to the Rust ``tonin`` crate. Same shape, same defaults, same
config knobs — different runtime. Designed so a polyglot team can move
between Rust and Python services without re-learning auth, state, jobs,
or telemetry.

All public I/O surfaces are ``async def``. There is no blocking variant.
The framework targets ``grpc.aio`` for the transport, ``asyncpg`` for
Postgres, ``redis.asyncio`` for Redis, ``httpx.AsyncClient`` for outbound
HTTP. Application code follows the same rule: every handler is
``async def``, every database call is awaited, no synchronous blocking
in the event loop.

Quick start::

    import tonin

    async def main():
        svc = tonin.Service.new("greeter").with_auth(tonin.auth.verifier())
        svc.handler(MyGreeterServicer(), add_GreeterServicer_to_server)
        await svc.run()

The :mod:`tonin_client` package holds shared types (``AuthCtx``,
``RetryPolicy``, ``CircuitBreaker``) — a peer service that only consumes
this service's generated client should depend on ``<svc>-client`` (which
pulls ``tonin-client``), not on ``tonin`` itself.
"""

from __future__ import annotations

from tonin import auth, job, mcp, state, telemetry  # noqa: F401  (re-export)
from tonin.service import Service
from tonin.state import State, StorageProvider

# Re-export common client types so users can write
# `from tonin import AuthCtx` without dipping into tonin_client.
from tonin_client.auth import AuthCtx, AuthError, PrincipalKind, RawToken

__all__ = [
    "AuthCtx",
    "AuthError",
    "PrincipalKind",
    "RawToken",
    "Service",
    "State",
    "StorageProvider",
    "auth",
    "job",
    "mcp",
    "state",
    "telemetry",
]

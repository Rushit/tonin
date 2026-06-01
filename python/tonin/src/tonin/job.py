"""Background-job entry point.

Python equivalent of ``tonin_core::job``. A "job" is a Python binary
(typically ``python -m <svc>_server.jobs.<job>``) that runs to
completion (queue consumer, scheduled task, one-shot migration runner)
rather than serving gRPC.

What :func:`bootstrap` does:

1. Initialize OTel (same :mod:`tonin.telemetry` the server uses).
2. Mint a **service-identity** :class:`AuthCtx` via an HTTP service
   token endpoint configured by ``TONIN_AUTH_SERVICE_TOKEN_URL``.
3. Build a :class:`tonin.State` from env (asyncpg + redis lazily).

Usage::

    import asyncio
    import tonin

    async def main():
        ctx = await tonin.job.bootstrap("greeter-cleanup")
        # ctx.state.pg / ctx.state.redis available
        # ctx.auth.propagate(metadata) on outbound calls

    asyncio.run(main())

**Async-by-default.** All I/O in this module is via ``httpx.AsyncClient``,
``asyncpg``, ``redis.asyncio``; there is no synchronous path.

Spawn pitfall: ``CURRENT_AUTH`` is a :mod:`contextvars` ``ContextVar`` set
by the **server** interceptor. Jobs don't set it (no inbound request),
so handler code that reads :func:`tonin.auth.current` from inside a
job gets the anonymous default. Use ``ctx.auth`` from this bootstrap's
return value instead, and pass it explicitly to any task you spawn.
"""

from __future__ import annotations

import logging
import os
import time
from dataclasses import dataclass

import httpx

from tonin import telemetry
from tonin.state import State
from tonin_client.auth import AuthCtx, AuthError, PrincipalKind

logger = logging.getLogger(__name__)


@dataclass(slots=True)
class JobCtx:
    """Output of :func:`bootstrap`: identity + pre-wired storage."""

    auth: AuthCtx
    state: State


async def bootstrap(name: str) -> JobCtx:
    """Initialize telemetry, mint a service-identity token, and resolve state.

    Designed to be the first line of a job binary's ``async def main``.

    :raises AuthError: ``TONIN_AUTH_SERVICE_TOKEN_URL`` is unset, or
        the auth service is unreachable. Both are deploy-time problems
        and the job should fail fast.
    :raises RuntimeError: ``DATABASE_URL`` / ``REDIS_URL`` set but the
        backing service is unreachable.
    """
    telemetry.init(name)
    logger.info("job bootstrapping name=%s", name)

    auth = await _mint_service_token(name)
    state = await State.from_env()

    logger.info(
        "job ready name=%s subject=%s has_pg=%s has_redis=%s",
        name,
        auth.subject,
        state.has_pg(),
        state.has_redis(),
    )
    return JobCtx(auth=auth, state=state)


async def _mint_service_token(service_name: str) -> AuthCtx:
    """POST to the configured mint endpoint and convert the response.

    Matches the Rust ``HttpServiceTokenMinter`` envelope:

    Request::

        POST $TONIN_AUTH_SERVICE_TOKEN_URL
        {"audience": "<aud>", "scopes": ["...", "..."]}

    Response::

        {"token": "<jwt>", "expires_in": <secs, optional>}
    """
    url = os.environ.get("TONIN_AUTH_SERVICE_TOKEN_URL")
    if not url:
        raise AuthError.config("TONIN_AUTH_SERVICE_TOKEN_URL unset")

    audience = os.environ.get("TONIN_AUTH_SERVICE_AUDIENCE") or service_name
    scopes_env = os.environ.get("TONIN_AUTH_SERVICE_TOKEN_SCOPES", "")
    scopes = [s.strip() for s in scopes_env.split(",") if s.strip()]

    try:
        async with httpx.AsyncClient(timeout=5.0) as http:
            resp = await http.post(url, json={"audience": audience, "scopes": scopes})
            resp.raise_for_status()
            body = resp.json()
    except httpx.HTTPError as e:
        raise AuthError.transport(str(e)) from e

    token = body.get("token")
    if not isinstance(token, str) or not token:
        raise AuthError.verification("mint response missing 'token'")

    expires_in = body.get("expires_in", 0)
    expires_at = float(time.time() + (expires_in if isinstance(expires_in, (int, float)) else 0))

    return AuthCtx(
        subject=service_name,
        audience=audience,
        scopes=scopes,
        kind=PrincipalKind.SERVICE,
        raw_token=token,
        expires_at=expires_at,
    )

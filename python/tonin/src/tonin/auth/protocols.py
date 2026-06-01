"""Auth-related protocols (interfaces).

Python equivalents of the Rust traits ``TokenExtractor``,
``TokenVerifier``, ``ServiceTokenMinter``. Defined as :mod:`typing`
``Protocol`` so users can drop in any class with matching ``async def``
methods without subclassing.
"""

from __future__ import annotations

from typing import Protocol, runtime_checkable

from tonin_client.auth import AuthCtx, AuthError, RawToken  # noqa: F401  (re-export in docs)


@runtime_checkable
class TokenExtractor(Protocol):
    """Pulls a :class:`RawToken` out of an incoming request's metadata.

    Default: :class:`tonin.auth.BearerHeaderExtractor` reads
    ``Authorization: Bearer <token>``. Implementations can read cookies,
    query strings, or any other source — the framework only requires
    that the result is a :class:`RawToken` (or raises :class:`AuthError`).

    Synchronous because metadata parsing is CPU-only.
    """

    def extract(self, metadata: list[tuple[str, str]]) -> RawToken: ...


@runtime_checkable
class TokenVerifier(Protocol):
    """Verifies a :class:`RawToken` and returns the resulting :class:`AuthCtx`.

    Default: :class:`tonin.auth.JwtValidator` (signature + exp + iss
    + aud via JWKS).

    Async because real verifiers usually need to fetch a JWKS,
    introspect against an auth service, or hit a cache.
    """

    async def verify(self, token: RawToken) -> AuthCtx: ...


@runtime_checkable
class ServiceTokenMinter(Protocol):
    """Mints an :class:`AuthCtx` representing this service (no user).

    Used by background jobs and queue consumers — see :mod:`tonin.job`.
    Default impl is HTTP-based and lands in a future revision.
    """

    async def mint(self) -> AuthCtx: ...

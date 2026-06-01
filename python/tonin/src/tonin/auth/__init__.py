"""Authentication and authorization layer for Python services.

Mirror of the Rust ``tonin_core::auth`` module. Three async protocols,
one shared concrete type:

- :class:`TokenExtractor` — where the token comes from on an incoming request
- :class:`TokenVerifier` — how the token gets validated → ``AuthCtx``
- :class:`ServiceTokenMinter` — how a service-identity token gets minted
  (for background jobs / queue consumers with no user request to
  propagate from)

``AuthCtx`` is the shared concrete type from :mod:`tonin_client.auth`
— same shape across Rust and Python so polyglot meshes don't drift.

Common case (one-liner)::

    svc = tonin.Service.new("my-svc").with_auth(tonin.auth.verifier())

The default :func:`verifier` returns :class:`JwtValidator.from_env`,
falling back to :class:`JwtValidator.insecure_dev` when
``TONIN_AUTH_INSECURE_DEV=1``.

Custom verifier::

    class OktaVerifier:
        async def verify(self, token):
            ...  # return AuthCtx
    svc = tonin.Service.new("my-svc").with_auth(OktaVerifier())

Reading the current request's auth from a handler::

    auth = tonin.auth.current()
    if not auth.is_anonymous():
        auth.require_scope("greeter:hello")
"""

from __future__ import annotations

from tonin.auth.default import (
    BearerHeaderExtractor,
    JwtValidator,
    verifier,
)
from tonin.auth.interceptor import AuthInterceptor, current
from tonin.auth.protocols import TokenExtractor, TokenVerifier, ServiceTokenMinter

# Re-export shared types from tonin_client so users can write
# `from tonin.auth import AuthCtx` directly.
from tonin_client.auth import AuthCtx, AuthError, PrincipalKind, RawToken

__all__ = [
    "AuthCtx",
    "AuthError",
    "AuthInterceptor",
    "BearerHeaderExtractor",
    "JwtValidator",
    "PrincipalKind",
    "RawToken",
    "ServiceTokenMinter",
    "TokenExtractor",
    "TokenVerifier",
    "current",
    "verifier",
]

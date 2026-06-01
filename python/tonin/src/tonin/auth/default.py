"""Default implementations of the auth protocols.

- :class:`BearerHeaderExtractor` — reads ``Authorization: Bearer <token>``
- :class:`JwtValidator` — full JWT validation (sig + exp + iss + aud)
  with JWKS caching, async fetch via httpx

The :func:`verifier` factory returns the default validator wired from
env, falling back to insecure-dev mode for local development.
"""

from __future__ import annotations

import asyncio
import logging
import os
import time
from dataclasses import dataclass, field
from typing import Any

import httpx
import jwt
from jwt import PyJWKClient

from tonin_client.auth import AuthCtx, AuthError, PrincipalKind, RawToken

logger = logging.getLogger(__name__)


# -----------------------------------------------------------------------------
# BearerHeaderExtractor
# -----------------------------------------------------------------------------

class BearerHeaderExtractor:
    """Reads ``Authorization: Bearer <token>`` from request metadata.

    Metadata format matches what grpc.aio exposes:
    ``[(key, value), ...]`` with lowercase keys.
    """

    def extract(self, metadata: list[tuple[str, str]]) -> RawToken:
        for key, value in metadata:
            if key.lower() == "authorization":
                # grpc.aio normalizes header keys to lowercase but doesn't
                # touch values. Accept "Bearer " and "bearer ".
                prefix = None
                if value.startswith("Bearer "):
                    prefix = "Bearer "
                elif value.startswith("bearer "):
                    prefix = "bearer "
                if prefix is None:
                    raise AuthError.missing_token()
                token = value[len(prefix):].strip()
                if not token:
                    raise AuthError.missing_token()
                return RawToken(value=token, kind="bearer-jwt")
        raise AuthError.missing_token()


# -----------------------------------------------------------------------------
# JwtValidator
# -----------------------------------------------------------------------------

@dataclass(slots=True)
class _JwksCache:
    """Cached JWKS keyed by URL. ``ttl_secs`` controls refresh."""

    ttl_secs: float = 300.0
    _cache: dict[str, tuple[float, PyJWKClient]] = field(default_factory=dict)
    _lock: asyncio.Lock = field(default_factory=asyncio.Lock)

    async def client(self, url: str) -> PyJWKClient:
        async with self._lock:
            now = time.time()
            entry = self._cache.get(url)
            if entry is not None and (now - entry[0]) < self.ttl_secs:
                return entry[1]
            # PyJWKClient does its own caching internally; we just
            # refresh the wrapper periodically so a key rotation
            # eventually picks up.
            client = PyJWKClient(url, cache_keys=True)
            self._cache[url] = (now, client)
            return client


class JwtValidator:
    """JWT signature + claim validation, async.

    Configuration via env:

    - ``TONIN_AUTH_ISSUER`` — expected ``iss`` (required)
    - ``TONIN_AUTH_AUDIENCE`` — expected ``aud`` (required)
    - ``TONIN_AUTH_JWKS_URL`` — JWKS endpoint (required)
    - ``TONIN_AUTH_INSECURE_DEV=1`` — skip everything; trust any token

    Insecure-dev mode emits a loud WARN and is for local development
    only — production deploys must set the three required vars.
    """

    def __init__(
        self,
        *,
        issuer: str,
        audience: str,
        jwks_url: str,
        leeway_secs: float = 60.0,
        algorithms: list[str] | None = None,
    ) -> None:
        self.issuer = issuer
        self.audience = audience
        self.jwks_url = jwks_url
        self.leeway_secs = leeway_secs
        self.algorithms = algorithms or ["RS256", "RS384", "RS512", "ES256", "ES384"]
        self._jwks = _JwksCache()
        self._insecure_dev = False

    @classmethod
    def from_env(cls) -> "JwtValidator":
        """Build from ``TONIN_AUTH_*`` env vars.

        Raises :class:`AuthError` if required vars are missing — call
        :meth:`insecure_dev` instead for local dev.
        """
        if os.environ.get("TONIN_AUTH_INSECURE_DEV") == "1":
            logger.warning(
                "TONIN_AUTH_INSECURE_DEV=1 — JWT signatures NOT verified. Local dev only."
            )
            return cls.insecure_dev()
        issuer = os.environ.get("TONIN_AUTH_ISSUER")
        if not issuer:
            raise AuthError.config(
                "TONIN_AUTH_ISSUER unset (set TONIN_AUTH_INSECURE_DEV=1 for dev)"
            )
        audience = os.environ.get("TONIN_AUTH_AUDIENCE")
        if not audience:
            raise AuthError.config("TONIN_AUTH_AUDIENCE unset")
        jwks_url = os.environ.get("TONIN_AUTH_JWKS_URL")
        if not jwks_url:
            raise AuthError.config("TONIN_AUTH_JWKS_URL unset")
        return cls(issuer=issuer, audience=audience, jwks_url=jwks_url)

    @classmethod
    def insecure_dev(cls) -> "JwtValidator":
        """**Local dev only.** Skip signature verification.

        Triggered by ``TONIN_AUTH_INSECURE_DEV=1`` or by calling this
        constructor directly. Every token is accepted; the resulting
        ``AuthCtx`` carries the unverified claims so handlers see
        something useful.
        """
        v = cls(issuer="insecure-dev", audience="insecure-dev", jwks_url="")
        v._insecure_dev = True
        return v

    async def verify(self, token: RawToken) -> AuthCtx:
        if self._insecure_dev:
            return _claims_to_ctx(_unsafe_decode(token.value), token.value)

        try:
            client = await self._jwks.client(self.jwks_url)
            # PyJWKClient.get_signing_key_from_jwt is sync HTTP via urllib.
            # Run in a thread so we don't block the event loop.
            signing_key = await asyncio.to_thread(
                client.get_signing_key_from_jwt, token.value
            )
            claims = jwt.decode(
                token.value,
                signing_key.key,
                algorithms=self.algorithms,
                audience=self.audience,
                issuer=self.issuer,
                leeway=self.leeway_secs,
            )
        except jwt.ExpiredSignatureError as e:
            raise AuthError.expired() from e
        except jwt.InvalidAudienceError as e:
            raise AuthError.audience(self.audience, "?") from e
        except jwt.InvalidIssuerError as e:
            raise AuthError.issuer(self.issuer, "?") from e
        except jwt.InvalidSignatureError as e:
            raise AuthError.signature() from e
        except jwt.PyJWTError as e:
            raise AuthError.verification(str(e)) from e
        except httpx.HTTPError as e:
            raise AuthError.transport(str(e)) from e

        return _claims_to_ctx(claims, token.value)


def _unsafe_decode(token: str) -> dict[str, Any]:
    """Decode without verifying. Used only by insecure-dev."""
    try:
        return jwt.decode(token, options={"verify_signature": False})
    except jwt.PyJWTError:
        return {}


def _claims_to_ctx(claims: dict[str, Any], raw_token: str) -> AuthCtx:
    """Map standard JWT claims into our shared :class:`AuthCtx` shape.

    Anything we don't have a typed field for lands in ``ctx.extra``.
    """
    typed_keys = {"sub", "iss", "aud", "scope", "scp", "exp"}
    scopes: list[str] = []
    scope_claim = claims.get("scope")
    if isinstance(scope_claim, str):
        scopes = scope_claim.split()
    elif isinstance(scope_claim, list):
        scopes = [str(s) for s in scope_claim]
    if not scopes:
        scp_claim = claims.get("scp")
        if isinstance(scp_claim, list):
            scopes = [str(s) for s in scp_claim]

    aud = claims.get("aud")
    if isinstance(aud, list):
        aud = aud[0] if aud else ""

    # Heuristic: token with no `sub` matching a user pattern but issued
    # to this service is a service token. We default to USER; real
    # services override this in their custom verifier.
    kind = PrincipalKind.USER if claims.get("sub") else PrincipalKind.ANONYMOUS

    return AuthCtx(
        subject=str(claims.get("sub", "")),
        issuer=str(claims.get("iss", "")),
        audience=str(aud or ""),
        scopes=scopes,
        kind=kind,
        raw_token=raw_token,
        expires_at=float(claims.get("exp", 0.0)),
        extra={k: v for k, v in claims.items() if k not in typed_keys},
    )


# -----------------------------------------------------------------------------
# Convenience factory
# -----------------------------------------------------------------------------

def verifier() -> JwtValidator:
    """Build the default verifier from env.

    Equivalent to::

        try:
            return JwtValidator.from_env()
        except AuthError:
            return JwtValidator.insecure_dev()

    The scaffold's ``server.py`` calls this so ``cargo run``-equivalent
    workflows work locally without auth config; production deploys set
    the real env vars and the same code path validates JWTs properly.
    """
    try:
        return JwtValidator.from_env()
    except AuthError as e:
        logger.warning("falling back to insecure-dev verifier: %s", e)
        return JwtValidator.insecure_dev()

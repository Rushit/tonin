"""Auth types shared between Python server and client.

These types are the contract between:

- the inbound auth interceptor (in ``tonin.auth``) which produces an
  ``AuthCtx`` from a verified token, and
- the outbound client SDK which copies the bearer token onto downstream
  requests via :meth:`AuthCtx.propagate`.

Custom verifiers (Okta, Cognito, API keys, cookies) are implemented on
the server side via the ``TokenVerifier`` protocol — but they all return
the same ``AuthCtx`` defined here.
"""

from __future__ import annotations

import enum
import time
from dataclasses import dataclass, field
from typing import Any, MutableMapping

import grpc


class PrincipalKind(str, enum.Enum):
    """Who is making the call. Lowercase values match the Rust enum."""

    USER = "user"
    SERVICE = "service"
    AGENT = "agent"
    ANONYMOUS = "anonymous"


@dataclass(slots=True)
class RawToken:
    """A token as extracted from a request, before verification.

    The framework doesn't assume JWT — ``value`` could be a session ID,
    API key, or anything else a server-side ``TokenVerifier`` knows how
    to handle. ``kind`` is a hint for verifiers that handle multiple
    token formats (conventions: ``"bearer-jwt"``, ``"api-key"``,
    ``"session-cookie"``, ``"basic-auth"``).
    """

    value: str
    kind: str = "bearer-jwt"


@dataclass(slots=True)
class AuthCtx:
    """Identity + claims for the current request.

    The single concrete type that flows through the framework. Custom
    claims live in ``extra``. ``raw_token`` is the verbatim token the
    inbound layer verified; outbound calls use it to propagate the
    caller's identity to the next hop.
    """

    subject: str = ""
    issuer: str = ""
    audience: str = ""
    scopes: list[str] = field(default_factory=list)
    kind: PrincipalKind = PrincipalKind.ANONYMOUS
    raw_token: str = ""
    expires_at: float = 0.0  # unix seconds
    extra: dict[str, Any] = field(default_factory=dict)

    @classmethod
    def anonymous(cls) -> "AuthCtx":
        """Empty ``AuthCtx`` for opt-out / no-auth flows."""
        return cls()

    @classmethod
    def from_bearer(cls, token: str) -> "AuthCtx":
        """Wrap a bearer token without verification.

        For client-side code that already has a token (e.g., from a
        login flow) and wants to hand it to the framework's outbound
        propagation. Server-side code receives ``AuthCtx`` from the
        interceptor and should not call this.
        """
        ctx = cls(raw_token=token, kind=PrincipalKind.USER)
        return ctx

    def is_anonymous(self) -> bool:
        return self.kind == PrincipalKind.ANONYMOUS

    def is_expired(self) -> bool:
        """Return ``True`` if the token has passed its recorded expiry.

        ``False`` for an anonymous context (``expires_at == 0.0``) — an
        unset expiry is treated as "no expiry recorded", not as expired.
        """
        if self.expires_at <= 0.0:
            return False
        return time.time() > self.expires_at

    def propagate(self, metadata: MutableMapping[str, str] | list[tuple[str, str]]) -> None:
        """Inject the bearer token into outbound gRPC metadata.

        Accepts either:

        - a mutable mapping (``dict``-like): sets ``"authorization"``
        - a list of ``(key, value)`` tuples: appends one

        grpc.aio uses the list form natively (``metadata=[(...)]`` kwarg
        on stub calls). Both forms are supported so callers don't need
        to convert.
        """
        if not self.raw_token:
            return
        header = f"Bearer {self.raw_token}"
        if isinstance(metadata, list):
            metadata.append(("authorization", header))
        else:
            metadata["authorization"] = header

    def require_scope(self, scope: str) -> None:
        """Authorize a single scope. Raises gRPC PermissionDenied if missing.

        Designed to be called from inside an async handler::

            ctx.require_scope("greeter:hello")

        Raises :class:`grpc.RpcError` indirectly via context.abort in
        the caller — we raise :class:`AuthError` here and the framework
        interceptor converts it.
        """
        if scope not in self.scopes:
            raise AuthError.insufficient_scope(scope)


class AuthError(Exception):
    """Auth-related failures.

    Categorized via :attr:`code` so the framework interceptor can map
    each to the right gRPC status:

    - ``"missing_token"``, ``"signature"``, ``"expired"``, ``"audience"``,
      ``"issuer"``, ``"verification"`` → ``UNAUTHENTICATED``
    - ``"insufficient_scope"`` → ``PERMISSION_DENIED``
    - ``"config"``, ``"transport"`` → ``INTERNAL``
    """

    def __init__(self, code: str, message: str, *, required_scope: str | None = None) -> None:
        super().__init__(message)
        self.code = code
        self.required_scope = required_scope

    @classmethod
    def missing_token(cls) -> "AuthError":
        return cls("missing_token", "no token in request")

    @classmethod
    def signature(cls) -> "AuthError":
        return cls("signature", "token signature invalid")

    @classmethod
    def expired(cls) -> "AuthError":
        return cls("expired", "token expired")

    @classmethod
    def audience(cls, expected: str, got: str) -> "AuthError":
        return cls("audience", f"audience mismatch: expected {expected}, got {got}")

    @classmethod
    def issuer(cls, expected: str, got: str) -> "AuthError":
        return cls("issuer", f"issuer mismatch: expected {expected}, got {got}")

    @classmethod
    def verification(cls, detail: str) -> "AuthError":
        return cls("verification", f"token verification failed: {detail}")

    @classmethod
    def insufficient_scope(cls, scope: str) -> "AuthError":
        return cls("insufficient_scope", f"insufficient scope: required {scope}", required_scope=scope)

    @classmethod
    def config(cls, detail: str) -> "AuthError":
        return cls("config", f"configuration error: {detail}")

    @classmethod
    def transport(cls, detail: str) -> "AuthError":
        return cls("transport", f"transport error contacting auth backend: {detail}")

    def to_grpc_status_code(self) -> grpc.StatusCode:
        """Map this error to a gRPC StatusCode."""
        if self.code == "insufficient_scope":
            return grpc.StatusCode.PERMISSION_DENIED
        if self.code in ("config", "transport"):
            return grpc.StatusCode.INTERNAL
        return grpc.StatusCode.UNAUTHENTICATED

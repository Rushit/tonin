"""Shared client-side primitives for generated micro service SDKs.

Peer services that just want to call another service depend on this
package (via the ``<service>-client`` package they import) — **not** on
the full ``tonin`` framework. Keep this package small and its surface
stable; everything here is a contract between framework-owned middleware
and hand-written caller code.

What lives here:

- :mod:`tonin_client.auth` — ``AuthCtx``, ``RawToken``, ``PrincipalKind``,
  ``AuthError``. Shape of the auth context the server-side framework
  produces and outbound clients inject into requests.
- :mod:`tonin_client.retry` — config types for retry policies. The actual
  retry mechanism (a grpc.aio interceptor) lives in the server framework.
- :mod:`tonin_client.breaker` — config types for circuit breakers.

What does NOT live here:

- JWT validation (server-side; ``tonin.auth``).
- JWKS fetching, the inbound interceptor, anything that opens server sockets.
"""

from tonin_client.auth import AuthCtx, AuthError, PrincipalKind, RawToken
from tonin_client.breaker import CircuitBreaker
from tonin_client.retry import Backoff, RetryPolicy

__all__ = [
    "AuthCtx",
    "AuthError",
    "Backoff",
    "CircuitBreaker",
    "PrincipalKind",
    "RawToken",
    "RetryPolicy",
]

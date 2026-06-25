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
- :mod:`tonin_client.retry` — config types for retry policies. Execution
  is delegated to the per-pod outbound sidecar proxy.
- :mod:`tonin_client.breaker` — config types for circuit breakers.
  Execution is delegated to the per-pod outbound sidecar proxy.
- :mod:`tonin_client.propagate` — W3C traceparent/tracestate and
  gRPC deadline header helpers for injecting into outbound metadata.

What does NOT live here:

- JWT validation (server-side; ``tonin.auth``).
- JWKS fetching, the inbound interceptor, anything that opens server sockets.
- Request coalescing, response caching, retry/breaker execution — those
  are handled by the outbound sidecar proxy so they work across all
  worker processes (Gunicorn, uWSGI, etc.).
"""

from tonin_client.auth import AuthCtx, AuthError, PrincipalKind, RawToken
from tonin_client.breaker import CircuitBreaker
from tonin_client.propagate import inject_deadline, inject_traceparent, inject_tracestate
from tonin_client.retry import Backoff, RetryPolicy

__all__ = [
    "AuthCtx",
    "AuthError",
    "Backoff",
    "CircuitBreaker",
    "PrincipalKind",
    "RawToken",
    "RetryPolicy",
    "inject_deadline",
    "inject_traceparent",
    "inject_tracestate",
]

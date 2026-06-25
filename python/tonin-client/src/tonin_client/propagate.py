"""Request-header helpers for outbound gRPC calls.

Provides:

- :func:`inject_traceparent` / :func:`inject_tracestate` — W3C distributed
  trace context propagation so callees can join the caller's span.
- :func:`inject_deadline` — shrinking deadline propagation via the
  ``grpc-timeout`` header so downstream hops never exceed the budget
  remaining from the edge.

All helpers accept either:

- a ``list[tuple[str, str]]`` — the native form for ``grpc.aio`` stub calls
  (``metadata=[(...)]`` kwarg).
- a ``dict[str, str]`` — simpler form for testing or synchronous stubs.

Returns ``True`` if the header was injected, ``False`` if the value was
invalid or the deadline already passed (caller can short-circuit).

W3C traceparent format: ``00-<32 hex>-<16 hex>-<2 hex>``
https://www.w3.org/TR/trace-context/
"""

from __future__ import annotations

import re
import time
import warnings

_TRACEPARENT_RE = re.compile(
    r"^00-[0-9a-f]{32}-[0-9a-f]{16}-[0-9a-f]{2}$",
    re.ASCII,
)

Metadata = list[tuple[str, str]] | dict[str, str]


def _inject(metadata: Metadata, key: str, value: str) -> None:
    if isinstance(metadata, list):
        metadata.append((key, value))
    else:
        metadata[key] = value


def inject_traceparent(metadata: Metadata, traceparent: str) -> bool:
    """Inject a W3C traceparent header.

    Validates the ``00-<trace-id>-<span-id>-<flags>`` format before
    injecting. Malformed values are dropped with a warning.
    """
    if not _TRACEPARENT_RE.match(traceparent):
        warnings.warn(
            f"malformed traceparent {traceparent!r} — not injected",
            stacklevel=2,
        )
        return False
    _inject(metadata, "traceparent", traceparent)
    return True


def inject_tracestate(metadata: Metadata, tracestate: str) -> bool:
    """Inject a W3C tracestate header (vendor-specific trace data).

    No format validation beyond non-empty — tracestate is opaque to the
    framework. Returns ``False`` for empty strings.
    """
    if not tracestate:
        return False
    _inject(metadata, "tracestate", tracestate)
    return True


def inject_deadline(metadata: Metadata, deadline_monotonic: float) -> bool:
    """Inject a ``grpc-timeout`` header from a monotonic deadline.

    Computes the remaining budget as ``deadline_monotonic - time.monotonic()``
    and encodes it in gRPC timeout format (e.g. ``"450m"`` for 450 ms).
    Returns ``False`` without injecting when the deadline has already passed,
    so callers can short-circuit immediately.

    Parameters
    ----------
    metadata:
        The outbound metadata to inject into.
    deadline_monotonic:
        A ``time.monotonic()`` value representing when the call must complete.
        Obtain from ``time.monotonic() + timeout_secs`` at the edge handler.
    """
    remaining = deadline_monotonic - time.monotonic()
    if remaining <= 0:
        return False
    _inject(metadata, "grpc-timeout", _format_grpc_timeout(remaining))
    return True


def _format_grpc_timeout(secs: float) -> str:
    """Encode seconds as a gRPC timeout header value.

    Picks the largest unit that represents the duration without loss of
    precision beyond whole units.

    gRPC timeout units: H hours · M minutes · S seconds · m milliseconds ·
    u microseconds · n nanoseconds
    """
    nanos = int(secs * 1_000_000_000)
    if nanos <= 0:
        return "0n"
    for divisor, unit in (
        (3_600_000_000_000, "H"),
        (60_000_000_000, "M"),
        (1_000_000_000, "S"),
        (1_000_000, "m"),
        (1_000, "u"),
    ):
        if nanos >= divisor and nanos % divisor == 0:
            return f"{nanos // divisor}{unit}"
    return f"{nanos}n"

"""Retry-policy configuration for outbound calls.

The actual retry mechanism (a grpc.aio.UnaryUnaryClientInterceptor that
consumes a :class:`RetryPolicy`) ships with the server framework — but
the **config type lives here** so generated client SDKs can expose the
knob to peer services without dragging in the framework.

Typical use (peer service)::

    from tonin_client.retry import RetryPolicy
    from greeter_client import GreeterClient

    client = GreeterClient(channel, retry=RetryPolicy.exponential(3))

Defaults:

The default policy is **no retries**. Retries change observable behavior
(duplicated side effects, amplified load on a slow callee); callers opt
in explicitly.
"""

from __future__ import annotations

import enum
from dataclasses import dataclass, field

import grpc


class Backoff(enum.Enum):
    EXPONENTIAL = "exponential"
    FIXED = "fixed"


def _default_retryable_codes() -> list[grpc.StatusCode]:
    """Safe-only retryable codes: transient infra failures.

    Adding ``INTERNAL`` or ``ABORTED`` to this set should be a deliberate
    call — they often mean "the server thinks it processed your request".
    """
    return [grpc.StatusCode.UNAVAILABLE, grpc.StatusCode.DEADLINE_EXCEEDED]


@dataclass(slots=True)
class RetryPolicy:
    """Retry behavior for an outbound RPC."""

    max_attempts: int = 1
    """Total attempts including the first. ``1`` = no retry."""

    initial_backoff_secs: float = 0.0
    """Delay before the first retry."""

    backoff: Backoff = Backoff.FIXED
    """How subsequent delays grow."""

    multiplier: float = 2.0
    """Exponential multiplier — ignored when ``backoff == FIXED``."""

    max_backoff_secs: float = 1.0
    """Cap on any single backoff interval, regardless of multiplier."""

    retryable_codes: list[grpc.StatusCode] = field(default_factory=_default_retryable_codes)
    """Which gRPC codes count as retryable."""

    @classmethod
    def none(cls) -> "RetryPolicy":
        """No retries — single attempt, fail fast. Default."""
        return cls()

    @classmethod
    def exponential(cls, max_attempts: int) -> "RetryPolicy":
        """Exponential backoff with sane defaults: 50ms → 100ms → 200ms."""
        return cls(
            max_attempts=max_attempts,
            initial_backoff_secs=0.05,
            backoff=Backoff.EXPONENTIAL,
            multiplier=2.0,
            max_backoff_secs=2.0,
        )

    @classmethod
    def fixed(cls, max_attempts: int, delay_secs: float) -> "RetryPolicy":
        """Fixed delay between attempts."""
        return cls(
            max_attempts=max_attempts,
            initial_backoff_secs=delay_secs,
            backoff=Backoff.FIXED,
            max_backoff_secs=delay_secs,
        )

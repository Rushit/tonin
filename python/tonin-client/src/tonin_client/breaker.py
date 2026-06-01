"""Circuit-breaker configuration for outbound calls.

Same split as :mod:`tonin_client.retry`: the active mechanism (an
async interceptor that tracks failure rates) ships with the framework,
but the config type lives here so peers can tune the breaker without
depending on the server framework.

State machine: standard three-state breaker (Closed → Open → HalfOpen →
Closed). See :class:`CircuitBreaker` fields for the knobs.
"""

from __future__ import annotations

from dataclasses import dataclass, field

import grpc


def _default_failure_codes() -> list[grpc.StatusCode]:
    return [grpc.StatusCode.UNAVAILABLE, grpc.StatusCode.DEADLINE_EXCEEDED]


@dataclass(slots=True)
class CircuitBreaker:
    """Breaker config for outbound RPCs.

    - **Closed**: traffic flows. Failures counted in a rolling window.
    - **Open**: failure rate crossed :attr:`trip_threshold`; calls
      short-circuit with ``UNAVAILABLE`` for :attr:`reset_after_secs`.
    - **HalfOpen**: :attr:`half_open_probes` requests get through; if
      any succeed, back to Closed; if any fail, back to Open.
    """

    window_secs: float = 10.0
    trip_threshold: float = 0.5
    """Failure ratio in ``(0, 1]`` at which the breaker trips."""

    min_requests: int = 20
    """Minimum requests in the window before the breaker can trip.

    Prevents tripping off a single isolated failure during low traffic.
    """

    reset_after_secs: float = 30.0
    half_open_probes: int = 3
    failure_codes: list[grpc.StatusCode] = field(default_factory=_default_failure_codes)

    @classmethod
    def aggressive(cls) -> "CircuitBreaker":
        """Trip fast — for non-critical RPCs where fail-fast beats queuing."""
        return cls(
            window_secs=5.0,
            trip_threshold=0.3,
            min_requests=5,
            reset_after_secs=15.0,
            half_open_probes=1,
        )

    @classmethod
    def conservative(cls) -> "CircuitBreaker":
        """Trip slow — for critical-path RPCs where queuing beats shedding."""
        return cls(
            window_secs=30.0,
            trip_threshold=0.75,
            min_requests=50,
            reset_after_secs=60.0,
            half_open_probes=5,
            failure_codes=[grpc.StatusCode.UNAVAILABLE],
        )

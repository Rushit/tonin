"""Unit tests for retry + breaker config types."""

from __future__ import annotations

import grpc

from tonin_client.breaker import CircuitBreaker
from tonin_client.retry import Backoff, RetryPolicy


def test_retry_default_is_no_retry() -> None:
    assert RetryPolicy().max_attempts == 1
    assert RetryPolicy.none().max_attempts == 1


def test_retry_exponential_defaults() -> None:
    p = RetryPolicy.exponential(3)
    assert p.max_attempts == 3
    assert p.backoff == Backoff.EXPONENTIAL
    assert p.multiplier > 1.0


def test_retry_retryable_codes_are_safe_only_by_default() -> None:
    p = RetryPolicy()
    assert grpc.StatusCode.UNAVAILABLE in p.retryable_codes
    assert grpc.StatusCode.INTERNAL not in p.retryable_codes
    assert grpc.StatusCode.ABORTED not in p.retryable_codes


def test_breaker_default_trip_threshold_is_half() -> None:
    cb = CircuitBreaker()
    assert abs(cb.trip_threshold - 0.5) < 1e-9


def test_breaker_aggressive_trips_faster_than_conservative() -> None:
    a = CircuitBreaker.aggressive()
    c = CircuitBreaker.conservative()
    assert a.min_requests < c.min_requests
    assert a.trip_threshold < c.trip_threshold

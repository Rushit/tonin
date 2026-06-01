"""Drift detection: hand-written types in auth.py / retry.py / breaker.py
must match the codegen output in _generated.py.

If this test fails, either:
  - Add the missing field to the hand-written type (Rust changed, run
    `cargo run --bin gen-shared-types` to refresh _generated.py first,
    then update auth.py / retry.py / breaker.py to match), or
  - If you intentionally changed the hand-written shape, update the
    Rust source and regenerate.

The Rust types are the source of truth. _generated.py mirrors them.
This test ensures the hand-written runtime types haven't drifted from
that mirror.
"""

from __future__ import annotations

import dataclasses

from tonin_client import auth as hand_auth
from tonin_client import breaker as hand_breaker
from tonin_client import retry as hand_retry
from tonin_client import _generated


def _field_set(cls) -> set[str]:
    return {f.name for f in dataclasses.fields(cls)}


def test_authctx_fields_match() -> None:
    assert _field_set(hand_auth.AuthCtx) == _field_set(_generated.AuthCtx)


def test_rawtoken_fields_match() -> None:
    assert _field_set(hand_auth.RawToken) == _field_set(_generated.RawToken)


def test_principal_kind_variants_match() -> None:
    hand_values = {p.value for p in hand_auth.PrincipalKind}
    gen_values = {p.value for p in _generated.PrincipalKind}
    assert hand_values == gen_values


def test_retry_policy_fields_match() -> None:
    # Hand-written RetryPolicy has an extra `retryable_codes` field
    # carrying grpc.StatusCode values (Python-specific). The codegen'd
    # interface covers only the cross-language-portable subset. Allow
    # the hand-written class to be a superset.
    gen_fields = _field_set(_generated.RetryPolicy)
    hand_fields = _field_set(hand_retry.RetryPolicy)
    missing_in_hand = gen_fields - hand_fields
    assert not missing_in_hand, (
        f"hand-written RetryPolicy is missing fields present in _generated: "
        f"{missing_in_hand}"
    )


def test_backoff_variants_match() -> None:
    # Hand-written uses uppercase NAMES, but the wire VALUES must match.
    hand_values = {b.value for b in hand_retry.Backoff}
    gen_values = {b.value for b in _generated.Backoff}
    assert hand_values == gen_values


def test_circuit_breaker_fields_match() -> None:
    # The hand-written CircuitBreaker has an extra `failure_codes` field
    # that's not codegen'd because it carries grpc.StatusCode values
    # (Python-specific). The codegen'd interface covers only the
    # cross-language-portable subset. Allow the hand-written class to
    # be a superset.
    gen_fields = _field_set(_generated.CircuitBreaker)
    hand_fields = _field_set(hand_breaker.CircuitBreaker)
    missing_in_hand = gen_fields - hand_fields
    assert not missing_in_hand, (
        f"hand-written CircuitBreaker is missing fields present in _generated: "
        f"{missing_in_hand}"
    )

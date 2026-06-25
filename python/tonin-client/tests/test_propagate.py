"""Unit tests for tonin_client.propagate — mirrors crates/tonin-client/src/propagate.rs tests."""

from __future__ import annotations

import time

import pytest

from tonin_client.propagate import (
    _format_grpc_timeout,
    inject_deadline,
    inject_traceparent,
    inject_tracestate,
)

VALID_TRACEPARENT = "00-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331-01"


# ---------------------------------------------------------------------------
# inject_traceparent
# ---------------------------------------------------------------------------


def test_inject_traceparent_into_list_metadata() -> None:
    metadata: list[tuple[str, str]] = []
    assert inject_traceparent(metadata, VALID_TRACEPARENT) is True
    assert ("traceparent", VALID_TRACEPARENT) in metadata


def test_inject_traceparent_into_dict_metadata() -> None:
    metadata: dict[str, str] = {}
    assert inject_traceparent(metadata, VALID_TRACEPARENT) is True
    assert metadata["traceparent"] == VALID_TRACEPARENT


def test_inject_malformed_traceparent_is_noop(recwarn: pytest.WarningsChecker) -> None:
    metadata: list[tuple[str, str]] = []
    result = inject_traceparent(metadata, "bad-value")
    assert result is False
    assert metadata == []
    assert len(recwarn) == 1


def test_inject_traceparent_wrong_version_rejected() -> None:
    metadata: list[tuple[str, str]] = []
    # Version must be "00"
    bad = "01-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331-01"
    assert inject_traceparent(metadata, bad) is False
    assert metadata == []


def test_inject_traceparent_uppercase_rejected() -> None:
    # W3C spec requires lowercase hex
    metadata: list[tuple[str, str]] = []
    upper = "00-0AF7651916CD43DD8448EB211C80319C-B7AD6B7169203331-01"
    assert inject_traceparent(metadata, upper) is False


# ---------------------------------------------------------------------------
# inject_tracestate
# ---------------------------------------------------------------------------


def test_inject_tracestate_into_list_metadata() -> None:
    metadata: list[tuple[str, str]] = []
    assert inject_tracestate(metadata, "vendor=value") is True
    assert ("tracestate", "vendor=value") in metadata


def test_inject_tracestate_into_dict_metadata() -> None:
    metadata: dict[str, str] = {}
    assert inject_tracestate(metadata, "vendor=value") is True
    assert metadata["tracestate"] == "vendor=value"


def test_inject_empty_tracestate_is_noop() -> None:
    metadata: list[tuple[str, str]] = []
    assert inject_tracestate(metadata, "") is False
    assert metadata == []


# ---------------------------------------------------------------------------
# _format_grpc_timeout
# ---------------------------------------------------------------------------


def test_format_timeout_picks_coarsest_lossless_unit() -> None:
    assert _format_grpc_timeout(3600.0) == "1H"
    assert _format_grpc_timeout(120.0) == "2M"
    assert _format_grpc_timeout(5.0) == "5S"
    assert _format_grpc_timeout(0.450) == "450m"
    assert _format_grpc_timeout(0.000_750) == "750u"
    assert _format_grpc_timeout(0.000_000_123) == "123n"


def test_format_timeout_sub_unit_boundary_uses_finer_unit() -> None:
    # 1.5 s is not a whole number of seconds → falls to millis
    assert _format_grpc_timeout(1.5) == "1500m"


# ---------------------------------------------------------------------------
# inject_deadline
# ---------------------------------------------------------------------------


def test_inject_deadline_future_writes_grpc_timeout_list() -> None:
    metadata: list[tuple[str, str]] = []
    deadline = time.monotonic() + 5.0
    assert inject_deadline(metadata, deadline) is True
    keys = [k for k, _ in metadata]
    assert "grpc-timeout" in keys


def test_inject_deadline_future_writes_grpc_timeout_dict() -> None:
    metadata: dict[str, str] = {}
    deadline = time.monotonic() + 5.0
    assert inject_deadline(metadata, deadline) is True
    assert "grpc-timeout" in metadata


def test_inject_deadline_past_returns_false() -> None:
    metadata: list[tuple[str, str]] = []
    past = time.monotonic() - 1.0
    assert inject_deadline(metadata, past) is False
    assert metadata == []


def test_inject_deadline_value_is_valid_grpc_timeout_format() -> None:
    metadata: dict[str, str] = {}
    inject_deadline(metadata, time.monotonic() + 2.0)
    value = metadata["grpc-timeout"]
    assert value[-1] in "HMSmun"
    assert value[:-1].isdigit()

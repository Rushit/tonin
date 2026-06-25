"""Unit tests for tonin_client.auth — mirrors crates/tonin-client tests."""

from __future__ import annotations

import grpc
import pytest

from tonin_client.auth import AuthCtx, AuthError, PrincipalKind


def test_anonymous_authctx_is_anonymous() -> None:
    a = AuthCtx.anonymous()
    assert a.is_anonymous()
    assert a.kind == PrincipalKind.ANONYMOUS


def test_from_bearer_carries_token() -> None:
    a = AuthCtx.from_bearer("abc.def.ghi")
    assert a.raw_token == "abc.def.ghi"
    assert a.kind == PrincipalKind.USER


def test_propagate_into_list_metadata() -> None:
    metadata: list[tuple[str, str]] = []
    AuthCtx.from_bearer("abc.def.ghi").propagate(metadata)
    assert ("authorization", "Bearer abc.def.ghi") in metadata


def test_propagate_into_dict_metadata() -> None:
    metadata: dict[str, str] = {}
    AuthCtx.from_bearer("abc.def.ghi").propagate(metadata)
    assert metadata["authorization"] == "Bearer abc.def.ghi"


def test_propagate_anonymous_is_noop() -> None:
    metadata: list[tuple[str, str]] = []
    AuthCtx.anonymous().propagate(metadata)
    assert metadata == []


def test_require_scope_ok_when_present() -> None:
    a = AuthCtx(scopes=["read:billing"])
    a.require_scope("read:billing")  # no raise


def test_require_scope_err_when_missing() -> None:
    a = AuthCtx.anonymous()
    with pytest.raises(AuthError) as exc:
        a.require_scope("admin")
    assert exc.value.code == "insufficient_scope"
    assert exc.value.required_scope == "admin"


def test_is_expired_false_for_anonymous() -> None:
    assert not AuthCtx.anonymous().is_expired()


def test_is_expired_false_when_future() -> None:
    import time
    ctx = AuthCtx(expires_at=time.time() + 3600.0)
    assert not ctx.is_expired()


def test_is_expired_true_when_past() -> None:
    ctx = AuthCtx(expires_at=1.0)  # 1970-01-01 — definitely past
    assert ctx.is_expired()


def test_auth_error_maps_to_correct_status() -> None:
    assert AuthError.signature().to_grpc_status_code() == grpc.StatusCode.UNAUTHENTICATED
    assert (
        AuthError.insufficient_scope("admin").to_grpc_status_code()
        == grpc.StatusCode.PERMISSION_DENIED
    )
    assert AuthError.config("missing env").to_grpc_status_code() == grpc.StatusCode.INTERNAL

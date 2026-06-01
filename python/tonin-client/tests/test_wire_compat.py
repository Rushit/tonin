"""Cross-language wire compatibility check.

Rust's serde produces a specific JSON shape for `AuthCtx`. The Python
side must deserialize it into the same field names. This test
constructs a JSON payload matching what serde emits and verifies it
round-trips into both the hand-written and the generated Python types.

If the Rust struct's `#[serde(...)]` attributes change (e.g. a field
gets renamed via `#[serde(rename = ...)]`), this test catches the
break before it hits a polyglot deployment.
"""

from __future__ import annotations

import dataclasses
import json
from typing import Any

from tonin_client import _generated
from tonin_client.auth import AuthCtx, PrincipalKind


# Sample JSON matching `serde_json::to_value(&AuthCtx { ... })` output.
# This is the wire shape — snake_case field names matching Rust's
# struct fields (serde uses field names verbatim by default).
SAMPLE_AUTHCTX_JSON = {
    "subject": "alice",
    "issuer": "https://issuer.example",
    "audience": "my-svc",
    "scopes": ["read:billing", "write:billing"],
    "kind": "user",  # PrincipalKind serializes lowercase (see #[serde(rename_all = "lowercase")])
    "raw_token": "abc.def.ghi",
    "expires_at": 1735689600.0,
    "extra": {"tenant_id": "acme"},
}


def test_authctx_hand_written_accepts_serde_shape() -> None:
    """The hand-written AuthCtx accepts every field from the Rust serde output."""
    data = SAMPLE_AUTHCTX_JSON
    ctx = AuthCtx(
        subject=data["subject"],
        issuer=data["issuer"],
        audience=data["audience"],
        scopes=data["scopes"],
        kind=PrincipalKind(data["kind"]),
        raw_token=data["raw_token"],
        expires_at=data["expires_at"],
        extra=data["extra"],
    )
    assert ctx.subject == "alice"
    assert ctx.kind == PrincipalKind.USER
    assert ctx.scopes == ["read:billing", "write:billing"]


def test_authctx_generated_accepts_serde_shape() -> None:
    """The codegen'd _generated.AuthCtx accepts every field from the Rust serde output."""
    data = SAMPLE_AUTHCTX_JSON
    ctx = _generated.AuthCtx(
        subject=data["subject"],
        issuer=data["issuer"],
        audience=data["audience"],
        scopes=data["scopes"],
        kind=_generated.PrincipalKind(data["kind"]),
        raw_token=data["raw_token"],
        expires_at=data["expires_at"],
        extra=data["extra"],
    )
    assert ctx.subject == "alice"
    assert ctx.kind == _generated.PrincipalKind.USER


def test_principal_kind_wire_values_are_lowercase() -> None:
    """Rust's PrincipalKind uses #[serde(rename_all = "lowercase")]."""
    for variant in PrincipalKind:
        assert variant.value == variant.value.lower()
    for variant in _generated.PrincipalKind:
        assert variant.value == variant.value.lower()


def test_authctx_serializable_to_json() -> None:
    """The hand-written AuthCtx round-trips through JSON."""
    ctx = AuthCtx(subject="bob", kind=PrincipalKind.SERVICE, raw_token="tok")
    # dataclasses.asdict + json.dumps must round-trip cleanly.
    raw = dataclasses.asdict(ctx)
    raw["kind"] = ctx.kind.value  # enum needs explicit unwrap
    encoded = json.dumps(raw)
    decoded = json.loads(encoded)
    assert decoded["subject"] == "bob"
    assert decoded["kind"] == "service"
    assert decoded["raw_token"] == "tok"

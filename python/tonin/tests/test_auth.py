"""Unit tests for tonin.auth — mirror of crates/tonin-core/src/auth tests."""

from __future__ import annotations

import time

import grpc
import jwt
import pytest
from cryptography.hazmat.primitives.asymmetric import rsa
from cryptography.hazmat.primitives import serialization

from tonin.auth import (
    AuthError,
    BearerHeaderExtractor,
    JwtValidator,
    PrincipalKind,
)
from tonin_client.auth import RawToken


# ----- BearerHeaderExtractor -----

def test_bearer_extractor_parses_authorization() -> None:
    ex = BearerHeaderExtractor()
    md = [("authorization", "Bearer abc.def.ghi")]
    tok = ex.extract(md)
    assert tok.value == "abc.def.ghi"
    assert tok.kind == "bearer-jwt"


def test_bearer_extractor_missing_header() -> None:
    ex = BearerHeaderExtractor()
    with pytest.raises(AuthError) as exc:
        ex.extract([])
    assert exc.value.code == "missing_token"


def test_bearer_extractor_wrong_scheme() -> None:
    ex = BearerHeaderExtractor()
    with pytest.raises(AuthError) as exc:
        ex.extract([("authorization", "Basic foo:bar")])
    assert exc.value.code == "missing_token"


# ----- JwtValidator -----

@pytest.fixture(scope="module")
def rsa_keypair() -> tuple[bytes, bytes]:
    """Generate an RSA keypair we use to sign + verify test tokens.

    Returned as PEM bytes — PyJWT accepts both PEM string and PEM bytes
    on the verify side; the encode side wants the private key object,
    which we re-load from PEM as needed inside each test.
    """
    private_key = rsa.generate_private_key(public_exponent=65537, key_size=2048)
    private_pem = private_key.private_bytes(
        encoding=serialization.Encoding.PEM,
        format=serialization.PrivateFormat.PKCS8,
        encryption_algorithm=serialization.NoEncryption(),
    )
    public_pem = private_key.public_key().public_bytes(
        encoding=serialization.Encoding.PEM,
        format=serialization.PublicFormat.SubjectPublicKeyInfo,
    )
    return private_pem, public_pem


def _make_token(private_pem: bytes, claims: dict) -> str:
    return jwt.encode(claims, private_pem, algorithm="RS256")


class _StaticValidator(JwtValidator):
    """Test-only: skip JWKS, use a fixed public key."""

    def __init__(self, public_pem: bytes, *, issuer: str, audience: str) -> None:
        super().__init__(issuer=issuer, audience=audience, jwks_url="")
        self._public_pem = public_pem

    async def verify(self, token: RawToken):  # type: ignore[no-untyped-def]
        # Re-implement the inner decode with our static key.
        try:
            claims = jwt.decode(
                token.value,
                self._public_pem,
                algorithms=["RS256"],
                audience=self.audience,
                issuer=self.issuer,
                leeway=self.leeway_secs,
            )
        except jwt.ExpiredSignatureError as e:
            raise AuthError.expired() from e
        except jwt.InvalidAudienceError as e:
            raise AuthError.audience(self.audience, "?") from e
        except jwt.InvalidIssuerError as e:
            raise AuthError.issuer(self.issuer, "?") from e
        except jwt.InvalidSignatureError as e:
            raise AuthError.signature() from e
        from tonin.auth.default import _claims_to_ctx

        return _claims_to_ctx(claims, token.value)


@pytest.mark.asyncio
async def test_jwt_validator_accepts_valid_token(rsa_keypair) -> None:
    private_pem, public_pem = rsa_keypair
    now = int(time.time())
    token_str = _make_token(
        private_pem,
        {
            "iss": "https://issuer.example",
            "aud": "my-svc",
            "sub": "alice",
            "iat": now,
            "exp": now + 3600,
            "scope": "read:billing write:billing",
        },
    )
    v = _StaticValidator(public_pem, issuer="https://issuer.example", audience="my-svc")
    ctx = await v.verify(RawToken(value=token_str))
    assert ctx.subject == "alice"
    assert ctx.kind == PrincipalKind.USER
    assert "read:billing" in ctx.scopes


@pytest.mark.asyncio
async def test_jwt_validator_rejects_expired_token(rsa_keypair) -> None:
    private_pem, public_pem = rsa_keypair
    # Beyond default 60s leeway.
    now = int(time.time())
    token_str = _make_token(
        private_pem,
        {
            "iss": "https://issuer.example",
            "aud": "my-svc",
            "sub": "alice",
            "iat": now - 7200,
            "exp": now - 3600,
        },
    )
    v = _StaticValidator(public_pem, issuer="https://issuer.example", audience="my-svc")
    with pytest.raises(AuthError) as exc:
        await v.verify(RawToken(value=token_str))
    assert exc.value.code == "expired"


@pytest.mark.asyncio
async def test_jwt_validator_rejects_wrong_audience(rsa_keypair) -> None:
    private_pem, public_pem = rsa_keypair
    now = int(time.time())
    token_str = _make_token(
        private_pem,
        {
            "iss": "https://issuer.example",
            "aud": "other-svc",
            "sub": "alice",
            "exp": now + 3600,
        },
    )
    v = _StaticValidator(public_pem, issuer="https://issuer.example", audience="my-svc")
    with pytest.raises(AuthError) as exc:
        await v.verify(RawToken(value=token_str))
    assert exc.value.code == "audience"


def test_jwt_validator_from_env_requires_config(monkeypatch) -> None:
    monkeypatch.delenv("TONIN_AUTH_INSECURE_DEV", raising=False)
    monkeypatch.delenv("TONIN_AUTH_ISSUER", raising=False)
    with pytest.raises(AuthError) as exc:
        JwtValidator.from_env()
    assert exc.value.code == "config"


def test_jwt_validator_from_env_insecure_dev(monkeypatch) -> None:
    monkeypatch.setenv("TONIN_AUTH_INSECURE_DEV", "1")
    v = JwtValidator.from_env()
    assert v._insecure_dev is True


@pytest.mark.asyncio
async def test_insecure_dev_accepts_any_token() -> None:
    v = JwtValidator.insecure_dev()
    token_str = jwt.encode(
        {"sub": "alice", "scope": "read:thing"}, "secret", algorithm="HS256"
    )
    ctx = await v.verify(RawToken(value=token_str))
    assert ctx.subject == "alice"
    assert ctx.kind == PrincipalKind.USER


# ----- error mapping -----

def test_auth_error_status_codes() -> None:
    assert AuthError.signature().to_grpc_status_code() == grpc.StatusCode.UNAUTHENTICATED
    assert (
        AuthError.insufficient_scope("admin").to_grpc_status_code()
        == grpc.StatusCode.PERMISSION_DENIED
    )
    assert AuthError.config("missing").to_grpc_status_code() == grpc.StatusCode.INTERNAL

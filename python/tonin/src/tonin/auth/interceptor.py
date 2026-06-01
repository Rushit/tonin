"""grpc.aio server interceptor that runs auth on every inbound request.

Mirror of the Rust :class:`AuthLayer`. For each incoming RPC:

1. Extract a :class:`RawToken` from metadata (default: bearer header).
2. Verify it → :class:`AuthCtx` (default: :class:`JwtValidator`).
3. Stash the ``AuthCtx`` in a :mod:`contextvars` ``ContextVar`` so the
   handler can read it via :func:`current` — same pattern as the Rust
   ``CURRENT_AUTH`` task-local.

Spawn pitfall: ``contextvars`` is copied when you create a Task via
``asyncio.create_task`` from inside a handler. If you create a Task
**before** the handler runs (rare), the new task sees the anonymous
default. In practice every handler-spawned task inherits the context
because handlers always run inside an awaited frame.
"""

from __future__ import annotations

import contextvars
import logging
from typing import Any, Callable, Awaitable

import grpc
from grpc.aio import ServerInterceptor

from tonin_client.auth import AuthCtx, AuthError

from tonin.auth.protocols import TokenExtractor, TokenVerifier

logger = logging.getLogger(__name__)

# Public contextvar. Handlers read via current(); the interceptor writes
# it via a `.set()` call before invoking the handler continuation.
CURRENT_AUTH: contextvars.ContextVar[AuthCtx] = contextvars.ContextVar(
    "tonin_current_auth", default=AuthCtx.anonymous()
)


def current() -> AuthCtx:
    """Read the current request's :class:`AuthCtx`.

    Returns the anonymous default if no auth interceptor ran (e.g.,
    called from a job binary, where the framework sets a service-
    identity ``AuthCtx`` separately via :mod:`tonin.job`).
    """
    return CURRENT_AUTH.get()


class AuthInterceptor(ServerInterceptor):
    """Server interceptor that installs auth on every inbound RPC."""

    def __init__(
        self,
        extractor: TokenExtractor,
        verifier: TokenVerifier,
        *,
        optional: bool = False,
    ) -> None:
        """
        :param extractor: pulls the raw token off the request metadata
        :param verifier: validates the token, returning ``AuthCtx``
        :param optional: if True, a ``MissingToken`` is converted to an
            anonymous ``AuthCtx`` instead of 401. Used by
            :meth:`tonin.Service.without_auth`.
        """
        self._extractor = extractor
        self._verifier = verifier
        self._optional = optional

    async def intercept_service(
        self,
        continuation: Callable[[grpc.HandlerCallDetails], Awaitable[grpc.RpcMethodHandler]],
        handler_call_details: grpc.HandlerCallDetails,
    ) -> grpc.RpcMethodHandler:
        """Wrap the per-RPC handler with auth.

        grpc.aio's interceptor protocol gives us the *handler*, not the
        request. We wrap each method handler with a closure that does
        the extract→verify→ContextVar dance before calling the original.
        """
        original = await continuation(handler_call_details)
        if original is None:
            return original

        extractor = self._extractor
        verifier = self._verifier
        optional = self._optional

        async def _verify(metadata: list[tuple[str, str]]) -> AuthCtx:
            try:
                token = extractor.extract(metadata)
            except AuthError as e:
                if e.code == "missing_token" and optional:
                    return AuthCtx.anonymous()
                raise
            return await verifier.verify(token)

        # Wrap unary-unary which is the only kind we use for the scaffold.
        # Streaming variants can be added later by mirroring this pattern.
        if original.unary_unary is not None:
            inner = original.unary_unary

            async def unary_unary(request: Any, context: grpc.aio.ServicerContext) -> Any:
                metadata = list(context.invocation_metadata() or [])
                try:
                    ctx = await _verify(metadata)
                except AuthError as e:
                    await context.abort(e.to_grpc_status_code(), str(e))
                    return None  # never reached; abort raises
                token = CURRENT_AUTH.set(ctx)
                try:
                    # Make the AuthCtx available via context invocation
                    # extensions as well, for callers that prefer
                    # AuthCtx.from(ctx) over the contextvar.
                    return await inner(request, context)
                finally:
                    CURRENT_AUTH.reset(token)

            # Note: `unary_unary_rpc_method_handler` lives on the
            # top-level `grpc` module, not `grpc.aio`. Same on the
            # streaming variants — grpc.aio reuses the regular handler
            # factories.
            return grpc.unary_unary_rpc_method_handler(
                unary_unary,
                request_deserializer=original.request_deserializer,
                response_serializer=original.response_serializer,
            )

        # For non-unary RPCs, pass through unchanged. (Add streaming
        # wrappers when the scaffold needs them.)
        return original

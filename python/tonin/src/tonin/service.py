"""Service builder — the Python equivalent of ``tonin_core::Service``.

Wraps :mod:`grpc.aio.Server` with a fluent builder that wires telemetry
and auth defaults. Async-by-default throughout: there is no sync
``run()``; the only way to start the server is ``await svc.run()``.

Usage::

    svc = (
        tonin.Service.new("greeter")
        .with_auth(tonin.auth.verifier())
        .handler(GreeterServicer(), add_GreeterServicer_to_server)
    )
    await svc.run()

The ``handler`` call accepts both:

- a ``servicer`` instance whose class implements the generated
  ``...Servicer`` ABC, and
- the matching ``add_..._to_server`` function (also generated).

The framework calls the function for you so the user doesn't have to
hold the gRPC ``Server`` object explicitly.
"""

from __future__ import annotations

import asyncio
import logging
from typing import Any, Callable

import grpc

from tonin import telemetry
from tonin.auth.default import BearerHeaderExtractor
from tonin.auth.interceptor import AuthInterceptor
from tonin.auth.protocols import TokenVerifier
from tonin.mcp import McpConfig, build_default_server, spawn as spawn_mcp
from tonin.mcp._grpc_bridge import expose_servicer_as_tools
from tonin_client.auth import AuthCtx

logger = logging.getLogger(__name__)


class _AnonymousVerifier:
    """Verifier that always returns anonymous. Used by :meth:`Service.without_auth`."""

    async def verify(self, token):  # type: ignore[no-untyped-def]
        return AuthCtx.anonymous()


class Service:
    """Builder for a gRPC server with auth + telemetry pre-wired."""

    def __init__(self, name: str) -> None:
        self._name = name
        self._addr = "0.0.0.0:50051"
        self._interceptor: AuthInterceptor | None = None
        # List of (servicer, add_to_server_fn) tuples to install in run().
        self._handlers: list[tuple[Any, Callable[..., None]]] = []
        # In-process MCP config (None = MCP disabled).
        self._mcp: McpConfig | None = None
        # Enable gzip compression on responses.
        self._enable_compression: bool = False
        # Side effect: install telemetry. Errors are logged inside.
        telemetry.init(name)

    @classmethod
    def new(cls, name: str) -> "Service":
        """Construct a new ``Service``. Initializes OTel as a side effect."""
        return cls(name)

    def addr(self, addr: str) -> "Service":
        """Override the bind address. Default ``0.0.0.0:50051``."""
        self._addr = addr
        return self

    def with_compression(self) -> "Service":
        """Enable gzip compression on responses for reduced bandwidth.
        Clients can control compression via accept-encoding header.
        """
        self._enable_compression = True
        return self

    def without_compression(self) -> "Service":
        """Disable gzip compression on responses."""
        self._enable_compression = False
        return self

    def enable_mcp(self) -> "Service":
        """Enable the in-process MCP listener on the default port
        (``0.0.0.0:50052``). Same process as the gRPC server, shares
        ``contextvars.CURRENT_AUTH`` and DB/cache/storage pools.

        Today this opens an HTTP listener that answers ``GET /health``
        with 200. The real FastMCP-backed bridge (each gRPC method as
        an MCP tool, JSON Schema auto-derived) lands in a follow-up.
        """
        self._mcp = McpConfig()
        return self

    def mcp_addr(self, host: str, port: int) -> "Service":
        """Enable the in-process MCP listener on a specific address.
        Useful for tests (`port=0` for OS-assigned random) or for
        non-default deployments.
        """
        self._mcp = McpConfig(host=host, port=port)
        return self

    def with_auth(self, verifier: TokenVerifier) -> "Service":
        """Install an auth verifier with the default bearer extractor.

        Every inbound request runs the extractor → verifier chain. The
        resulting :class:`AuthCtx` is placed in the ``CURRENT_AUTH``
        context variable for the handler's execution.
        """
        self._interceptor = AuthInterceptor(
            extractor=BearerHeaderExtractor(),
            verifier=verifier,
            optional=False,
        )
        return self

    def without_auth(self) -> "Service":
        """Run all requests as anonymous.

        Handlers can still read :func:`tonin.auth.current` — they'll
        get an ``AuthCtx`` with ``kind=PrincipalKind.ANONYMOUS``.
        """
        self._interceptor = AuthInterceptor(
            extractor=BearerHeaderExtractor(),
            verifier=_AnonymousVerifier(),
            optional=True,
        )
        return self

    def handler(
        self,
        servicer: Any,
        add_to_server: Callable[..., None],
    ) -> "Service":
        """Register a servicer + its ``add_..._to_server`` function."""
        self._handlers.append((servicer, add_to_server))
        return self

    async def run(self) -> None:
        """Start the server and block until shutdown.

        Calls :func:`telemetry.shutdown` in a ``finally`` so SIGINT
        still flushes the last batch of spans.
        """
        if not self._handlers:
            raise RuntimeError("no handler registered")
        interceptors: list[grpc.aio.ServerInterceptor] = []
        if self._interceptor is not None:
            interceptors.append(self._interceptor)
        else:
            # Default to anonymous-but-tracked, same as Rust's
            # `default_anonymous_auth`.
            interceptors.append(
                AuthInterceptor(
                    BearerHeaderExtractor(),
                    _AnonymousVerifier(),
                    optional=True,
                )
            )

        # Configure compression options (gzip enabled by default).
        # In gRPC Python, compression is negotiated via client accept-encoding.
        options: list[tuple[str, Any]] = []
        if self._enable_compression:
            options.append(
                ("grpc.default_compression_algorithm", "gzip"),
            )

        server = grpc.aio.server(interceptors=interceptors, options=options if options else None)
        for servicer, add_to_server in self._handlers:
            add_to_server(servicer, server)
        server.add_insecure_port(self._addr)

        # In-process MCP listener (if enabled). Same posture as Rust:
        # if MCP fails to bind, the service still serves gRPC.
        mcp_task: asyncio.Task | None = None
        if self._mcp is not None:
            try:
                # Build the FastMCP instance up-front so we can attach
                # auto-derived tools BEFORE the listener starts.
                mcp_inst = build_default_server(self._mcp)
                for servicer, add_to_server in self._handlers:
                    try:
                        expose_servicer_as_tools(mcp_inst, servicer, add_to_server)
                    except Exception as e:  # noqa: BLE001
                        logger.warning(
                            "failed to derive MCP tools from %s: %s",
                            type(servicer).__name__,
                            e,
                        )
                (host, port), mcp_task = await spawn_mcp(self._mcp, mcp=mcp_inst)
                logger.info(
                    "mcp listener running service=%s host=%s port=%s",
                    self._name,
                    host,
                    port,
                )
            except Exception as e:  # noqa: BLE001
                logger.error("mcp listener failed to bind — continuing without MCP: %s", e)

        logger.info("micro service starting service=%s addr=%s", self._name, self._addr)
        await server.start()
        try:
            await server.wait_for_termination()
        finally:
            await server.stop(grace=5.0)
            if mcp_task is not None:
                mcp_task.cancel()
            telemetry.shutdown()

"""OpenTelemetry bootstrap.

Mirrors ``tonin-telemetry`` on the Rust side. Idempotent — calling
:func:`init` more than once is a no-op. Disabled when the environment
variable ``TONIN_TELEMETRY=off`` is set.

Exports:

- **Traces** via OTLP/gRPC to ``OTEL_EXPORTER_OTLP_ENDPOINT`` (default
  ``http://localhost:4317``).
- **Auto-instrumentation** for grpc.aio (both server + client side) so
  every RPC gets a span without app code doing anything.

The service name becomes the ``service.name`` resource attribute on
every span; this is what shows up as the row label in Jaeger / Tempo /
Grafana / Honeycomb.
"""

from __future__ import annotations

import logging
import os
import threading

logger = logging.getLogger(__name__)

_init_lock = threading.Lock()
_initialized = False


def init(service_name: str) -> None:
    """Wire up OTLP exporters + grpc.aio auto-instrumentation.

    Idempotent. Errors are logged at WARN — a misconfigured collector
    must NOT prevent the service from starting (we'd rather lose
    telemetry than refuse traffic).
    """
    global _initialized
    if _initialized:
        return
    if os.environ.get("TONIN_TELEMETRY") == "off":
        _initialized = True
        return

    with _init_lock:
        if _initialized:
            return
        try:
            from opentelemetry import trace
            from opentelemetry.exporter.otlp.proto.grpc.trace_exporter import (
                OTLPSpanExporter,
            )
            from opentelemetry.sdk.resources import Resource
            from opentelemetry.sdk.trace import TracerProvider
            from opentelemetry.sdk.trace.export import BatchSpanProcessor

            endpoint = os.environ.get(
                "OTEL_EXPORTER_OTLP_ENDPOINT", "http://localhost:4317"
            )
            resource = Resource.create({"service.name": service_name})
            provider = TracerProvider(resource=resource)
            exporter = OTLPSpanExporter(endpoint=endpoint, insecure=True)
            provider.add_span_processor(BatchSpanProcessor(exporter))
            trace.set_tracer_provider(provider)

            # Auto-instrument grpc.aio. The instrumentor wraps both
            # server.start() and client channels so every RPC gets a
            # span. Safe to call exactly once.
            try:
                from opentelemetry.instrumentation.grpc import (
                    GrpcAioInstrumentorClient,
                    GrpcAioInstrumentorServer,
                )

                GrpcAioInstrumentorServer().instrument()
                GrpcAioInstrumentorClient().instrument()
            except Exception as e:  # noqa: BLE001  (best-effort instrumentation)
                logger.warning("grpc.aio auto-instrumentation failed: %s", e)

            _initialized = True
        except Exception as e:  # noqa: BLE001  (telemetry must not block startup)
            logger.warning("telemetry init failed: %s", e)
            _initialized = True  # don't keep retrying


def shutdown() -> None:
    """Flush pending spans before process exit.

    The framework's ``Service.run`` calls this in a ``finally`` so a
    Ctrl-C still ships the last few traces.
    """
    try:
        from opentelemetry import trace

        provider = trace.get_tracer_provider()
        if hasattr(provider, "shutdown"):
            provider.shutdown()
    except Exception:  # noqa: BLE001
        pass

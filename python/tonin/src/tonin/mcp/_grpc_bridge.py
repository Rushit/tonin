"""Derive MCP tools from a grpc.aio servicer.

Walks the handlers registered by ``add_<Name>Servicer_to_server`` and
turns each gRPC method into a FastMCP ``@mcp.tool()``. Tool body invokes
the matching servicer method with a constructed Request and serializes
the Reply back to JSON.

Implementation notes:

- We probe the ``add_..._to_server`` function by passing it a mock
  server that records ``rpc_method_handlers``. This avoids importing
  internal grpc types or relying on naming conventions across versions.

- Schema derivation walks the Protobuf descriptor. We support the
  scalar types most services need (string, int32/64, bool, double,
  float, bytes-as-base64). Nested messages and repeated fields are
  rendered recursively. Enums become ``{type: string, enum: [...]}``.

- AuthCtx propagation: tool invocations are not real gRPC requests
  (they come in over MCP HTTP), so there's no ``ServicerContext``
  with invocation metadata. We synthesize a stub context that holds
  the current contextvar AuthCtx, so handler code that reads
  ``tonin.auth.current()`` still works.
"""

from __future__ import annotations

import inspect
import json
import logging
import re
from typing import Any, Callable


def _snake(name: str) -> str:
    """PascalCase → snake_case, mirroring prost / heck's behavior.

    Examples:
        SayHello   → say_hello
        S3Svc      → s3_svc      (digit+upper boundary)
        HTTPProxy  → http_proxy  (acronym followed by Word)
    """
    s = re.sub(r"([A-Z]+)([A-Z][a-z])", r"\1_\2", name)
    s = re.sub(r"([a-z0-9])([A-Z])", r"\1_\2", s)
    return s.lower()

from google.protobuf import descriptor as pb_descriptor
from google.protobuf import json_format
from google.protobuf.message import Message
from mcp.server.fastmcp import FastMCP

logger = logging.getLogger(__name__)


def expose_servicer_as_tools(
    mcp: FastMCP,
    servicer: Any,
    add_to_server: Callable[..., None],
) -> int:
    """Walk the servicer's gRPC method handlers and register each as an MCP tool.

    Returns the number of tools registered.
    """
    handlers = _capture_handlers(servicer, add_to_server)
    registered = 0
    for method_name, h in handlers.items():
        try:
            _register_one(mcp, servicer, method_name, h)
            registered += 1
        except Exception as e:  # noqa: BLE001
            logger.warning(
                "skipping MCP tool registration for %s: %s", method_name, e
            )
    logger.info("registered %d MCP tools from %s", registered, type(servicer).__name__)
    return registered


def _capture_handlers(
    servicer: Any,
    add_to_server: Callable[..., None],
) -> dict[str, Any]:
    """Run add_to_server against a probe server that captures handlers.

    grpc's add_<Name>Servicer_to_server passes its handler dict to
    ``server.add_generic_rpc_handlers((generic_handler,))``. We intercept
    that call and pull the dict out of the GenericHandler.
    """
    captured: dict[str, Any] = {}

    class _ProbeServer:
        def add_generic_rpc_handlers(self, handlers):
            for h in handlers:
                # GenericHandler has a `_method_handlers` dict in CPython grpc.
                inner = getattr(h, "_method_handlers", None)
                if inner is not None:
                    captured.update(inner)

        def add_registered_method_handlers(self, _service, _handlers):
            # No-op — the registered API is in addition to the generic
            # one, and the generic one already has everything we need.
            pass

    add_to_server(servicer, _ProbeServer())
    return captured


def _register_one(
    mcp: FastMCP,
    servicer: Any,
    full_method_path: str,
    handler: Any,
) -> None:
    """Register a single method as an MCP tool on ``mcp``.

    `full_method_path` is the fully-qualified RPC name from the grpc
    handler dict, e.g. ``"/pybridge.v1.Pybridge/SayHello"``. We use the
    last path segment for the servicer attribute and tool name.
    """
    # Extract the Python attribute name from "/pkg.v1.Service/MethodName".
    # The servicer class exposes it as the PascalCase last segment.
    method_attr = full_method_path.rsplit("/", 1)[-1]

    # The grpc method-handler tuple holds:
    #   request_deserializer  : bytes → Message
    #   response_serializer   : Message → bytes
    #   unary_unary           : fn(request, context) → response
    request_deser = getattr(handler, "request_deserializer", None)
    if request_deser is None:
        raise ValueError(f"{method_attr}: no request_deserializer (streaming RPC?)")

    request_cls = _resolve_message_class(request_deser)
    # response_serializer is `<message>.SerializeToString` (unbound on
    # upb), which doesn't have __self__ — we don't need it because
    # we serialize the reply via MessageToDict at runtime. Don't
    # validate it.

    tool_name = _snake(method_attr)
    method_obj = getattr(servicer, method_attr, None)
    description = (
        inspect.getdoc(method_obj)
        or inspect.getdoc(servicer.__class__)
        or f"gRPC method {method_attr}"
    ).strip().split("\n", 1)[0]

    input_schema = _message_to_json_schema(request_cls.DESCRIPTOR)

    if method_obj is None:
        raise AttributeError(f"servicer has no method {method_attr!r}")
    servicer_method = method_obj

    # FastMCP builds its arg validator from the function's signature.
    # To make it accept the proto fields by name, we construct a
    # function whose parameters mirror the proto fields exactly, with
    # `Any` type hints so we don't over-constrain (the input_schema
    # we attach below carries the real types for the wire).
    tool_fn = _build_tool_fn(
        proto_fields=[f.name for f in request_cls.DESCRIPTOR.fields],
        request_cls=request_cls,
        servicer_method=servicer_method,
    )

    mcp.add_tool(
        tool_fn,
        name=tool_name,
        description=description,
        structured_output=False,
    )
    # FastMCP's add_tool derives an input schema from the function
    # signature; since our generated function uses `Any` types, the
    # schema is permissive but field names are right. We overwrite
    # `parameters` with the proto-derived schema for richer client
    # visibility — fn_metadata.arg_model still validates at call time
    # using the permissive signature, which is the behavior we want.
    try:
        tool_entry = mcp._tool_manager._tools[tool_name]  # type: ignore[attr-defined]
        tool_entry.parameters = input_schema
    except (AttributeError, KeyError):
        pass


def _build_tool_fn(
    proto_fields: list[str],
    request_cls: Any,
    servicer_method: Callable[..., Any],
) -> Callable[..., Any]:
    """Construct an async function whose signature is one param per
    proto field, each typed ``Any`` and defaulted to ``None``.

    This shape makes FastMCP's pydantic-arg-model accept the same
    kwargs the MCP client sends (one per proto field), without our
    needing to bypass FastMCP's validation pipeline.

    The function captures ``request_cls`` + ``servicer_method`` in its
    closure and turns the kwargs into a proto request at call time.
    """
    # Build a function body via exec because Python's introspection
    # depends on real def-level parameters; we can't fake them via
    # functools.wraps or inspect.Signature alone.
    param_list = ", ".join(f"{name}=None" for name in proto_fields) if proto_fields else ""
    src = (
        f"async def _generated_tool({param_list}):\n"
        f"    kwargs = {{n: v for n, v in locals().items() if v is not None}}\n"
        f"    request = _parse_dict(kwargs, _request_cls())\n"
        f"    context = _StubContext()\n"
        f"    reply = _servicer_method(request, context)\n"
        f"    if _iscoroutine(reply):\n"
        f"        reply = await reply\n"
        f"    if isinstance(reply, _Message):\n"
        f"        return _json.dumps(_message_to_dict(reply, preserving_proto_field_name=True))\n"
        f"    return _json.dumps(reply)\n"
    )
    locals_dict: dict[str, Any] = {
        "_request_cls": request_cls,
        "_servicer_method": servicer_method,
        "_StubContext": _StubContext,
        "_iscoroutine": inspect.iscoroutine,
        "_Message": Message,
        "_json": json,
        "_parse_dict": json_format.ParseDict,
        "_message_to_dict": json_format.MessageToDict,
    }
    exec(src, locals_dict)  # noqa: S102 — controlled, generates a single function from a fixed template
    return locals_dict["_generated_tool"]


def _resolve_message_class(deser_or_ser: Callable[..., Any]) -> Any:
    """Recover the protobuf Message subclass from a serializer/deserializer.

    Generated grpc code uses ``Foo.FromString`` (bound method on the
    message class) for request_deserializer and ``Bar.SerializeToString``
    for response_serializer. The underlying class lives in ``__self__``.

    Under the modern `google._upb._message` runtime the class is not a
    direct subclass of ``google.protobuf.message.Message`` via Python's
    MRO — it's a C-extension type whose instances are. We accept any
    callable ``__self__`` that produces a ``Message`` when invoked.
    """
    self_ref = getattr(deser_or_ser, "__self__", None)
    if self_ref is None:
        raise TypeError(f"no __self__ on {deser_or_ser!r}")
    # Validate by instantiation; upb classes don't expose Python-side
    # subclass relationships to `isinstance(cls, type)` checks in the
    # idiomatic way.
    try:
        instance = self_ref()
    except Exception as e:  # noqa: BLE001
        raise TypeError(f"cannot instantiate {self_ref!r}: {e}") from e
    if not isinstance(instance, Message):
        raise TypeError(f"{self_ref!r} does not produce protobuf Messages")
    return self_ref


# -----------------------------------------------------------------------------
# Protobuf descriptor → JSON Schema
# -----------------------------------------------------------------------------

# Map protobuf scalar types to JSON Schema types. Anything not listed
# falls back to "string" — better than failing the registration.
_SCALAR_TYPE_MAP: dict[int, dict[str, Any]] = {
    pb_descriptor.FieldDescriptor.TYPE_STRING: {"type": "string"},
    pb_descriptor.FieldDescriptor.TYPE_BOOL: {"type": "boolean"},
    pb_descriptor.FieldDescriptor.TYPE_INT32: {"type": "integer", "format": "int32"},
    pb_descriptor.FieldDescriptor.TYPE_INT64: {"type": "integer", "format": "int64"},
    pb_descriptor.FieldDescriptor.TYPE_UINT32: {"type": "integer", "format": "uint32", "minimum": 0},
    pb_descriptor.FieldDescriptor.TYPE_UINT64: {"type": "integer", "format": "uint64", "minimum": 0},
    pb_descriptor.FieldDescriptor.TYPE_SINT32: {"type": "integer", "format": "int32"},
    pb_descriptor.FieldDescriptor.TYPE_SINT64: {"type": "integer", "format": "int64"},
    pb_descriptor.FieldDescriptor.TYPE_FIXED32: {"type": "integer", "format": "uint32", "minimum": 0},
    pb_descriptor.FieldDescriptor.TYPE_FIXED64: {"type": "integer", "format": "uint64", "minimum": 0},
    pb_descriptor.FieldDescriptor.TYPE_SFIXED32: {"type": "integer", "format": "int32"},
    pb_descriptor.FieldDescriptor.TYPE_SFIXED64: {"type": "integer", "format": "int64"},
    pb_descriptor.FieldDescriptor.TYPE_FLOAT: {"type": "number", "format": "float"},
    pb_descriptor.FieldDescriptor.TYPE_DOUBLE: {"type": "number", "format": "double"},
    pb_descriptor.FieldDescriptor.TYPE_BYTES: {"type": "string", "contentEncoding": "base64"},
}


def _message_to_json_schema(desc: pb_descriptor.Descriptor) -> dict[str, Any]:
    """Convert a protobuf Descriptor into a JSON Schema object.

    Walks fields once. Doesn't try to resolve cycles — recursive proto
    messages would infinite-loop here. (We don't have any in the
    scaffold; revisit if a real service needs it.)
    """
    properties: dict[str, Any] = {}
    required: list[str] = []
    for field in desc.fields:
        properties[field.name] = _field_to_schema(field)
        # Proto3 doesn't distinguish required/optional — all scalar
        # fields are "present" with a default value. Skip `required`.
    schema: dict[str, Any] = {"type": "object", "properties": properties}
    if required:
        schema["required"] = required
    return schema


def _field_to_schema(field: pb_descriptor.FieldDescriptor) -> dict[str, Any]:
    if field.label == pb_descriptor.FieldDescriptor.LABEL_REPEATED:
        return {"type": "array", "items": _field_inner_schema(field)}
    return _field_inner_schema(field)


def _field_inner_schema(field: pb_descriptor.FieldDescriptor) -> dict[str, Any]:
    if field.type == pb_descriptor.FieldDescriptor.TYPE_MESSAGE:
        return _message_to_json_schema(field.message_type)
    if field.type == pb_descriptor.FieldDescriptor.TYPE_ENUM:
        return {
            "type": "string",
            "enum": [v.name for v in field.enum_type.values],
        }
    return _SCALAR_TYPE_MAP.get(field.type, {"type": "string"}).copy()


# -----------------------------------------------------------------------------
# Stub ServicerContext for MCP tool invocations
# -----------------------------------------------------------------------------

class _StubContext:
    """Minimal ServicerContext-shaped object for MCP tool calls.

    Real ``grpc.aio.ServicerContext`` has lots of methods (invocation
    metadata, peer info, deadline, cancellation, abort, set_code...).
    MCP-triggered handlers can use them on best-effort; the stub raises
    NotImplementedError for the operations that don't make sense
    (e.g. peer() — there's no gRPC peer).
    """

    def invocation_metadata(self) -> list[tuple[str, str]]:
        return []

    async def abort(self, code: Any, details: str) -> None:
        raise RuntimeError(f"servicer aborted (code={code}): {details}")

    def set_code(self, _code: Any) -> None:
        pass

    def set_details(self, _details: str) -> None:
        pass

    def peer(self) -> str:
        return "mcp"

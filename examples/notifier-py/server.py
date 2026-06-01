"""notifier — Python implementation of a tonin service.

tonin is language-agnostic at the wire: any language with gRPC + Linkerd
sidecar interop can register against the same mesh. This is the reference
Python service.

Run: python server.py
Codegen: `python -m grpc_tools.protoc -I proto --python_out=. --grpc_python_out=. proto/notifier.proto`
"""
import asyncio
import logging
import grpc

# These imports come from generated code (run the codegen command above).
# Kept as soft imports so the file is readable without running protoc.
try:
    import notifier_pb2 as pb        # noqa: F401
    import notifier_pb2_grpc as rpc  # noqa: F401
    HAVE_GENERATED = True
except ImportError:
    HAVE_GENERATED = False


class NotifierImpl:  # would inherit rpc.NotifierServicer once generated
    async def SendOrderConfirmation(self, request, context):
        logging.info("order %s confirmed for customer %s", request.order_id, request.customer_id)
        return pb.Ack(ok=True)


async def serve() -> None:
    logging.basicConfig(level=logging.INFO)
    if not HAVE_GENERATED:
        logging.warning("generated proto modules missing; run protoc first")
        return
    server = grpc.aio.server()
    rpc.add_NotifierServicer_to_server(NotifierImpl(), server)
    server.add_insecure_port("0.0.0.0:50051")  # mTLS is terminated by Linkerd sidecar
    await server.start()
    logging.info("notifier listening on :50051")
    await server.wait_for_termination()


if __name__ == "__main__":
    asyncio.run(serve())

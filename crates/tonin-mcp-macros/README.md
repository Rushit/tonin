# tonin-mcp-macros

Proc-macro that derives an MCP tool surface from a gRPC service impl.

Part of the [tonin](https://crates.io/crates/tonin) framework.

## Don't depend on this directly

Use [`tonin`](https://crates.io/crates/tonin) and write `#[tonin::mcp_expose]`.
The macro is re-exported from the umbrella crate; depending on this crate
directly will skip the rmcp runtime hookup that lives in
[`tonin-core`](https://crates.io/crates/tonin-core).

## What it generates

Placed on a gRPC service impl block, `#[mcp_expose]`:

1. Re-emits the original `impl` block unchanged, so tonic still sees it.
2. Emits a sibling `<Impl>McpAdapter` struct holding `Arc<Impl>`.
3. For each `async fn name(&self, req: Request<ReqT>) -> Result<Response<RespT>, Status>`,
   emits an rmcp `#[tool]` method on the adapter that deserializes the
   MCP call into `ReqT`, dispatches into `Impl::name`, and serializes
   the response body back as `CallToolResult` text content.
4. Wires the adapter into rmcp's `#[tool_router]` + `#[tool_handler]`
   so `tonin_core::mcp::spawn_with(_, || Ok(GreeterImplMcpAdapter::new(...)))`
   serves it over streamable-HTTP on the MCP port.

Streaming RPCs and non-gRPC-shaped methods are skipped silently.
`ReqT` / `RespT` must derive `serde::Deserialize + serde::Serialize +
schemars::JsonSchema` — the scaffold's tonic-build config already does this.

## Example

Before:

```rust,ignore
#[tonic::async_trait]
impl Greeter for GreeterImpl {
    async fn say_hello(
        &self,
        req: Request<HelloRequest>,
    ) -> Result<Response<HelloReply>, Status> {
        Ok(Response::new(HelloReply {
            message: format!("hi {}", req.into_inner().name),
        }))
    }
}
```

After (one attribute added):

```rust,ignore
#[tonic::async_trait]
#[tonin::mcp_expose]
impl Greeter for GreeterImpl {
    async fn say_hello(
        &self,
        req: Request<HelloRequest>,
    ) -> Result<Response<HelloReply>, Status> {
        Ok(Response::new(HelloReply {
            message: format!("hi {}", req.into_inner().name),
        }))
    }
}
```

The same code now serves both gRPC on `:50051` and MCP on `:50052`,
with a `say_hello` tool that LLM clients can list and call.

## License

Licensed under the Apache License, Version 2.0.

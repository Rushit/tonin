//! Proc-macro that derives an MCP tool surface from a gRPC service impl.
//!
//! # Don't depend on this crate directly
//!
//! Use [`tonin`] and write `#[tonin::mcp_expose]`. The umbrella crate
//! re-exports this macro alongside the rmcp runtime it plugs into.
//!
//! # Example
//!
//! Before — a normal tonic service impl:
//!
//! ```rust,ignore
//! #[tonic::async_trait]
//! impl Greeter for GreeterImpl {
//!     async fn say_hello(
//!         &self,
//!         req: Request<HelloRequest>,
//!     ) -> Result<Response<HelloReply>, Status> { /* ... */ }
//! }
//! ```
//!
//! After — one attribute added:
//!
//! ```rust,ignore
//! #[tonic::async_trait]
//! #[tonin::mcp_expose]
//! impl Greeter for GreeterImpl {
//!     async fn say_hello(
//!         &self,
//!         req: Request<HelloRequest>,
//!     ) -> Result<Response<HelloReply>, Status> { /* ... */ }
//! }
//! ```
//!
//! Every gRPC method is now also an MCP tool, dispatched through a generated
//! `GreeterImplMcpAdapter` that `Service::handler` wires into the in-process
//! MCP server.
//!
//! # Sample app
//!
//! <https://github.com/Rushit/tonin/tree/main/examples/greeter>
//!
//! [`tonin`]: https://crates.io/crates/tonin

use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use quote::{format_ident, quote};
use syn::{
    FnArg, GenericArgument, ImplItem, ImplItemFn, ItemImpl, PathArguments, ReturnType, Type,
    TypePath, parse_macro_input,
};

#[proc_macro_attribute]
pub fn mcp_expose(_attr: TokenStream, item: TokenStream) -> TokenStream {
    let input = parse_macro_input!(item as ItemImpl);

    // The Self type of the impl block — `GreeterImpl` in the example.
    let self_ty = &input.self_ty;
    let self_ident = extract_type_ident(self_ty).unwrap_or_else(|| format_ident!("Impl"));
    let adapter_ident = format_ident!("{}McpAdapter", self_ident);

    // Walk methods, build per-method tool entries.
    let mut tool_methods = Vec::new();
    let mut skipped = Vec::new();
    for item in &input.items {
        if let ImplItem::Fn(method) = item {
            match parse_grpc_method(method) {
                Some(spec) => tool_methods.push(emit_adapter_tool(self_ty, &spec)),
                None => skipped.push(method.sig.ident.to_string()),
            }
        }
    }
    if tool_methods.is_empty() {
        let msg = format!(
            "mcp_expose: no gRPC-shaped methods found. Methods seen: [{}]. \
             Expected each to be `async fn name(&self, req: Request<X>) -> Result<Response<Y>, Status>`.",
            skipped.join(", ")
        );
        return syn::Error::new_spanned(self_ty, msg)
            .to_compile_error()
            .into();
    }

    // rmcp's `#[tool_router]` / `#[tool_handler]` macros pattern-match
    // on `#[tool]` by attribute NAME, not by full path. So we have to
    // emit bare `#[tool(...)]` and bring the macros into scope at the
    // expansion site. We do that via a `use rmcp::...` inside the
    // emitted code — the user's crate has `rmcp` + `rmcp-macros` as
    // deps via the scaffold's Cargo.toml.
    let adapter = quote! {
        // -------------------------------------------------------------
        // tonin generated MCP adapter for the impl block above.
        // -------------------------------------------------------------
        use ::rmcp::{tool, tool_router, tool_handler, ServerHandler as __ToninMcpServerHandler};

        #[derive(::std::clone::Clone)]
        pub struct #adapter_ident {
            inner: ::std::sync::Arc<#self_ty>,
            tool_router: ::rmcp::handler::server::router::tool::ToolRouter<#adapter_ident>,
        }

        impl #adapter_ident {
            pub fn new(inner: #self_ty) -> Self {
                Self {
                    inner: ::std::sync::Arc::new(inner),
                    tool_router: Self::tool_router(),
                }
            }
        }

        #[tool_router]
        impl #adapter_ident {
            #(#tool_methods)*
        }

        #[tool_handler]
        impl __ToninMcpServerHandler for #adapter_ident {
            fn get_info(&self) -> ::rmcp::model::ServerInfo {
                ::rmcp::model::ServerInfo::new(
                    ::rmcp::model::ServerCapabilities::builder().enable_tools().build(),
                )
                .with_server_info(::rmcp::model::Implementation::from_build_env())
                .with_protocol_version(::rmcp::model::ProtocolVersion::V_2024_11_05)
                .with_instructions(
                    "tonin service MCP endpoint. Tools auto-derived from gRPC methods.".to_string(),
                )
            }
        }
    };

    let expanded = quote! {
        #input
        #adapter
    };

    expanded.into()
}

struct GrpcMethodSpec {
    rust_name: syn::Ident,
    description: String,
    request_type: Type,
}

fn parse_grpc_method(method: &ImplItemFn) -> Option<GrpcMethodSpec> {
    let sig = &method.sig;
    sig.asyncness.as_ref()?;
    let mut inputs = sig.inputs.iter();
    match inputs.next() {
        Some(FnArg::Receiver(_)) => {}
        _ => return None,
    }
    let second = inputs.next()?;
    let pat_type = match second {
        FnArg::Typed(t) => t,
        _ => return None,
    };
    let request_type = extract_request_inner(&pat_type.ty)?;
    // Strict return-type check was rejecting valid `std::result::Result<...>`
    // shapes in observed user code. The signal that this is a gRPC method
    // is already strong from the first two params (&self + Request<X>) —
    // just check that there IS a return type at all.
    if !matches!(&sig.output, ReturnType::Type(_, _)) {
        return None;
    }
    let rust_name = sig.ident.clone();
    let description =
        extract_doc_comment(method).unwrap_or_else(|| format!("gRPC method {rust_name}"));
    Some(GrpcMethodSpec {
        rust_name,
        description,
        request_type,
    })
}

fn extract_type_ident(ty: &Type) -> Option<syn::Ident> {
    if let Type::Path(TypePath { path, .. }) = ty {
        return path.segments.last().map(|s| s.ident.clone());
    }
    None
}

fn extract_request_inner(ty: &Type) -> Option<Type> {
    let path = match ty {
        Type::Path(TypePath { path, .. }) => path,
        _ => return None,
    };
    let last = path.segments.last()?;
    if last.ident != "Request" {
        return None;
    }
    let args = match &last.arguments {
        PathArguments::AngleBracketed(a) => a,
        _ => return None,
    };
    for arg in &args.args {
        if let GenericArgument::Type(t) = arg {
            return Some(t.clone());
        }
    }
    None
}

fn extract_doc_comment(method: &ImplItemFn) -> Option<String> {
    for attr in &method.attrs {
        if attr.path().is_ident("doc")
            && let syn::Meta::NameValue(nv) = &attr.meta
            && let syn::Expr::Lit(syn::ExprLit {
                lit: syn::Lit::Str(s),
                ..
            }) = &nv.value
        {
            return Some(s.value().trim().to_string());
        }
    }
    None
}

/// Emit the rmcp `#[tool]` method that wraps a single user gRPC method.
///
/// The body calls into the user's impl via `self.inner.{method}(...)`,
/// then serializes the response message to JSON and returns it as a
/// rmcp text-content CallToolResult.
fn emit_adapter_tool(self_ty: &Type, spec: &GrpcMethodSpec) -> TokenStream2 {
    let method = &spec.rust_name;
    let description = &spec.description;
    let req_ty = &spec.request_type;
    quote! {
        #[tool(description = #description)]
        async fn #method(
            &self,
            ::rmcp::handler::server::wrapper::Parameters(req): ::rmcp::handler::server::wrapper::Parameters<#req_ty>,
        ) -> ::std::result::Result<::rmcp::model::CallToolResult, ::rmcp::ErrorData> {
            let inner: ::std::sync::Arc<#self_ty> = ::std::sync::Arc::clone(&self.inner);
            let tonic_req = ::tonic::Request::new(req);
            match inner.#method(tonic_req).await {
                Ok(resp) => {
                    let body = resp.into_inner();
                    let body_json = ::serde_json::to_string(&body)
                        .map_err(|e| ::rmcp::ErrorData::internal_error(
                            format!("serialize response: {}", e),
                            ::std::option::Option::None,
                        ))?;
                    Ok(::rmcp::model::CallToolResult::success(vec![
                        ::rmcp::model::Content::text(body_json),
                    ]))
                }
                Err(status) => Err(::rmcp::ErrorData::internal_error(
                    status.message().to_string(),
                    ::std::option::Option::None,
                )),
            }
        }
    }
}

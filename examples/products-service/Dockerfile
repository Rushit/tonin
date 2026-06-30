# syntax=docker/dockerfile:1

# --------------------------------------------------------------------------
# Builder. products-service has no crate dependencies, but it's a member of
# the tonin Cargo workspace, so Cargo still needs every other member's
# manifest (and a stub src/ where that manifest itself declares further
# path deps) to resolve the workspace graph. Build context is the workspace
# root.
# --------------------------------------------------------------------------
FROM rust:1.93-slim-bookworm AS builder

WORKDIR /build

# ---- manifest layer (cached until Cargo.lock changes) --------------------
COPY Cargo.toml Cargo.lock ./

COPY crates/tonin/Cargo.toml               crates/tonin/Cargo.toml
COPY crates/tonin-plugin/Cargo.toml        crates/tonin-plugin/Cargo.toml
COPY crates/tonin-sdk/Cargo.toml           crates/tonin-sdk/Cargo.toml
COPY crates/tonin-client/Cargo.toml        crates/tonin-client/Cargo.toml
COPY crates/tonin-mcp-macros/Cargo.toml    crates/tonin-mcp-macros/Cargo.toml
COPY crates/tonin-build/Cargo.toml         crates/tonin-build/Cargo.toml
COPY crates/tonin-config-etcd/Cargo.toml   crates/tonin-config-etcd/Cargo.toml
COPY crates/tonin-config-github/Cargo.toml crates/tonin-config-github/Cargo.toml
COPY crates/tonin-proxy/Cargo.toml         crates/tonin-proxy/Cargo.toml
COPY examples/greeter/Cargo.toml           examples/greeter/Cargo.toml
COPY examples/orders/Cargo.toml            examples/orders/Cargo.toml
COPY examples/inventory/Cargo.toml         examples/inventory/Cargo.toml
COPY examples/users-service/Cargo.toml     examples/users-service/Cargo.toml
COPY examples/orders-service/Cargo.toml    examples/orders-service/Cargo.toml
COPY examples/products-service/Cargo.toml  examples/products-service/Cargo.toml

# Stub every other workspace member's src/ so Cargo resolves the dep graph
# without needing their full source trees.
RUN set -eu; \
    for lib in \
        crates/tonin-plugin crates/tonin-sdk crates/tonin-client \
        crates/tonin-build crates/tonin-config-etcd crates/tonin-config-github; \
    do mkdir -p "${lib}/src" && echo "// stub" > "${lib}/src/lib.rs"; done; \
    for bin in \
        crates/tonin crates/tonin-proxy examples/greeter examples/orders \
        examples/inventory examples/users-service examples/orders-service; \
    do mkdir -p "${bin}/src" && echo "fn main(){}" > "${bin}/src/main.rs"; done; \
    mkdir -p crates/tonin-mcp-macros/src && echo "// stub" > crates/tonin-mcp-macros/src/lib.rs; \
    for b in examples/greeter examples/orders examples/inventory crates/tonin-sdk; \
    do echo "fn main(){}" > "${b}/build.rs"; done

# ---- source layer ----------------------------------------------------------
COPY examples/products-service/src/ examples/products-service/src/
RUN cargo build --release --bin products-service

# --------------------------------------------------------------------------
# Runtime — distroless/cc-debian12: dynamic glibc runtime, no shell, non-root.
# --------------------------------------------------------------------------
FROM gcr.io/distroless/cc-debian12:nonroot

COPY --from=builder /build/target/release/products-service /usr/local/bin/products-service

EXPOSE 50053

ENTRYPOINT ["/usr/local/bin/products-service"]

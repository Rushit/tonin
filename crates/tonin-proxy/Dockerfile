# syntax=docker/dockerfile:1

# --------------------------------------------------------------------------
# Builder — glibc debian-slim builder to align with host and CI compilation.
# Final image uses distroless/cc (dynamic glibc support, no shell, non-root).
# Typical result: ~35 MB uncompressed.
# --------------------------------------------------------------------------
FROM rust:1.90-slim-bookworm AS builder

# protobuf-compiler: required by etcd-client's build.rs (transitive dep).
# binutils: provides 'strip' to shrink the dynamic binary.
RUN apt-get update && apt-get install -y --no-install-recommends \
    protobuf-compiler \
    binutils \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /build

# ---- manifest layer (cached until Cargo.lock changes) --------------------
COPY Cargo.toml Cargo.lock ./

COPY crates/tonin/Cargo.toml             crates/tonin/Cargo.toml
COPY crates/tonin-plugin/Cargo.toml      crates/tonin-plugin/Cargo.toml
COPY crates/tonin-sdk/Cargo.toml         crates/tonin-sdk/Cargo.toml
COPY crates/tonin-client/Cargo.toml      crates/tonin-client/Cargo.toml
COPY crates/tonin-mcp-macros/Cargo.toml  crates/tonin-mcp-macros/Cargo.toml
COPY crates/tonin-build/Cargo.toml       crates/tonin-build/Cargo.toml
COPY crates/tonin-config-etcd/Cargo.toml crates/tonin-config-etcd/Cargo.toml
COPY crates/tonin-config-github/Cargo.toml crates/tonin-config-github/Cargo.toml
COPY crates/tonin-proxy/Cargo.toml       crates/tonin-proxy/Cargo.toml
COPY examples/greeter/Cargo.toml         examples/greeter/Cargo.toml
COPY examples/orders/Cargo.toml          examples/orders/Cargo.toml
COPY examples/inventory/Cargo.toml       examples/inventory/Cargo.toml

# Stub every workspace member's src/ so Cargo resolves the dep graph without
# needing their full source trees.
RUN set -eu; \
    for lib in \
        crates/tonin-plugin crates/tonin-sdk crates/tonin-client \
        crates/tonin-build crates/tonin-config-etcd crates/tonin-config-github; \
    do mkdir -p "${lib}/src" && echo "// stub" > "${lib}/src/lib.rs"; done; \
    for bin in crates/tonin examples/greeter examples/orders examples/inventory; \
    do mkdir -p "${bin}/src" && echo "fn main(){}" > "${bin}/src/main.rs"; done; \
    mkdir -p crates/tonin-mcp-macros/src && echo "// stub" > crates/tonin-mcp-macros/src/lib.rs; \
    for b in examples/greeter examples/orders examples/inventory crates/tonin-sdk; \
    do echo "fn main(){}" > "${b}/build.rs"; done

# Pre-build deps (cached layer).
RUN cargo build --profile proxy-release -p tonin-proxy 2>&1 | tail -3

# ---- source layer --------------------------------------------------------
COPY crates/tonin-proxy/src/ crates/tonin-proxy/src/
RUN touch crates/tonin-proxy/src/main.rs && \
    cargo build --profile proxy-release -p tonin-proxy && \
    strip target/proxy-release/tonin-proxy

# --------------------------------------------------------------------------
# Runtime — distroless/cc-debian12: dynamic glibc runtime with no shell or
# package manager. Provides libgcc, glibc, and CA certificates.
# --------------------------------------------------------------------------
FROM gcr.io/distroless/cc-debian12:nonroot

COPY --from=builder /build/target/proxy-release/tonin-proxy /usr/local/bin/tonin-proxy

EXPOSE 6565

ENTRYPOINT ["/usr/local/bin/tonin-proxy"]

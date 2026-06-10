//! tonin-transport: thin wrapper around tonic's gRPC server/client.
//!
//! Encryption (mTLS) is handled by the service mesh sidecar (Linkerd/Istio),
//! not in-process. This keeps service binaries small and lets the mesh own
//! cert rotation.

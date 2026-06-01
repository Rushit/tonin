# tonin-config-etcd

etcd v3 backend for the [tonin](https://crates.io/crates/tonin)
`Config` capability: dynamic application config with true push-based hot
reload, TLS-friendly for the in-cluster etcd pattern.

Implements `tonin_core::traits::Config`.

## When to use

Pick the etcd backend when:

- You already run etcd (or k3s/k8s, which ships one) and want a single
  source of truth for service config.
- You need real-time reload — etcd pushes events to subscribers as soon
  as a `PUT` or `DELETE` commits, no polling.
- You want to centralize tuning knobs across many services without
  redeploying every time you flip a flag.

For "secret" material (DB passwords, JWT signing keys, OAuth client
secrets) use the `SecretStore` trait + a dedicated secret backend —
`Config` is for non-secret application config.

## Quick example

```rust,no_run
use std::time::Duration;
use tonin_config_etcd::EtcdConfig;
use tonin_core::traits::Config;

# async fn run() -> Result<(), tonin_core::Error> {
let cfg = EtcdConfig::builder()
    .endpoint("https://etcd.kube-system.svc.cluster.local:2379")
    .prefix("/tonin/greeter/")
    .connect()
    .await?;

// One-shot read:
let pool_max = cfg.get("db.pool.max").await?;

// Live reload — receiver emits the current value, then re-emits on every
// etcd change. `interval` is ignored (etcd is push-based).
let mut rx = cfg.watch("feature.shadow_writes", Duration::from_secs(0));
tokio::spawn(async move {
    while rx.changed().await.is_ok() {
        let v = rx.borrow().clone();
        println!("feature.shadow_writes -> {v:?}");
    }
});
# Ok(()) }
```

`EtcdConfig::from_env()` reads endpoints + TLS + auth from
`TONIN_CONFIG_ETCD_*` env vars for the typical "configured at boot via
k8s env" deployment.

## TLS

Point the builder (or env vars) at PEM files on disk. The CA cert
verifies the etcd server's TLS cert; the client cert + key are optional
for mTLS.

| Env var                       | Meaning                              |
|-------------------------------|--------------------------------------|
| `TONIN_CONFIG_ETCD_ENDPOINTS` | Comma-separated `https://...` list   |
| `TONIN_CONFIG_ETCD_PREFIX`    | Prepended to every key path          |
| `TONIN_CONFIG_ETCD_TLS_CA`    | Path to PEM CA cert                  |
| `TONIN_CONFIG_ETCD_TLS_CERT`  | Path to PEM client cert (mTLS)       |
| `TONIN_CONFIG_ETCD_TLS_KEY`   | Path to PEM client key (mTLS)        |
| `TONIN_CONFIG_ETCD_USER`      | Basic-auth username                  |
| `TONIN_CONFIG_ETCD_PASSWORD`  | Basic-auth password                  |

Supplying the TLS client cert without the key (or vice versa) is a
configuration error caught at `connect()` time.

## Watch resilience

The watch task survives transient etcd outages: it logs a `tracing::warn!`
and reconnects with exponential backoff (1s, 2s, 4s, 8s, 16s, 30s). It
exits cleanly once the last receiver is dropped, freeing the etcd
connection.

## Status

Pre-alpha. Crate scaffold exists, public surface (`EtcdConfig`,
`EtcdConfigBuilder`, `TlsConfig`) is stable for 0.1, watch semantics are
push-based and reload-on-change. Integration tests are gated behind
`#[ignore]` because CI has no etcd — run with `--include-ignored`
against a local docker etcd.

## See also

- [tonin](https://crates.io/crates/tonin) — umbrella crate.
- [tonin-core](https://crates.io/crates/tonin-core) — defines the
  `Config` trait and `EnvConfig` / `ChainedConfig` default impls.

---

Licensed under the Apache License, Version 2.0.

//! Integration test against a real etcd instance.
//!
//! These tests are `#[ignore]` by default because CI doesn't ship an etcd
//! cluster. To run locally:
//!
//! ```bash
//! docker run --rm -d --name tonin-etcd-test \
//!   -p 2379:2379 -p 2380:2380 \
//!   -e ALLOW_NONE_AUTHENTICATION=yes \
//!   bitnami/etcd:3.5
//! TONIN_CONFIG_ETCD_ENDPOINTS=http://127.0.0.1:2379 \
//!   cargo test -p tonin-config-etcd --test etcd_round_trip -- --include-ignored
//! docker stop tonin-etcd-test
//! ```

use std::time::Duration;

use tonin_config_etcd::EtcdConfig;
use tonin_sdk::traits::Config;

#[tokio::test]
#[ignore = "requires an etcd instance reachable at TONIN_CONFIG_ETCD_ENDPOINTS"]
async fn round_trip_get_and_watch() {
    let endpoints = std::env::var("TONIN_CONFIG_ETCD_ENDPOINTS")
        .expect("TONIN_CONFIG_ETCD_ENDPOINTS must be set for this ignored test");

    let key = format!("round_trip_{}", std::process::id());
    let cfg = EtcdConfig::builder()
        .endpoints(
            endpoints
                .split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string),
        )
        .prefix("/tonin-test/")
        .connect()
        .await
        .expect("connect to etcd");

    // Initially absent.
    assert_eq!(cfg.get(&key).await.unwrap(), None);

    // Subscribe before any writes so we see the initial None then the
    // first put event.
    let mut rx = cfg.watch(&key, Duration::from_secs(1));
    // First emission is the seed value (None).
    rx.changed().await.expect("seed value");
    assert!(rx.borrow().is_none());

    // Write via a separate client (the trait surface is read-only) using
    // the raw etcd-client API. We can reuse the same connection through
    // the Config builder's connect step.
    let mut writer = etcd_client::Client::connect(
        endpoints.split(',').map(str::trim).collect::<Vec<_>>(),
        None,
    )
    .await
    .expect("connect writer");
    let full_key = format!("/tonin-test/{key}");
    writer
        .put(full_key.as_bytes().to_vec(), b"hello".to_vec(), None)
        .await
        .expect("put");

    // Watch should see the new value within a few seconds.
    tokio::time::timeout(Duration::from_secs(5), rx.changed())
        .await
        .expect("watch saw put")
        .expect("sender alive");
    assert_eq!(rx.borrow().as_deref(), Some(b"hello".as_ref()));

    // get() should now report the same value.
    assert_eq!(
        cfg.get(&key).await.unwrap().as_deref(),
        Some(b"hello".as_ref())
    );

    // Delete and verify watch sees None again.
    writer
        .delete(full_key.as_bytes().to_vec(), None)
        .await
        .expect("delete");
    tokio::time::timeout(Duration::from_secs(5), rx.changed())
        .await
        .expect("watch saw delete")
        .expect("sender alive");
    assert!(rx.borrow().is_none());
}

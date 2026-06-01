//! Cache capability — KV read/write/delete with TTL + conditional write.

use std::time::Duration;

use async_trait::async_trait;

use crate::Error;

#[async_trait]
pub trait Cache: Send + Sync + 'static {
    async fn get(&self, key: &str) -> Result<Option<Vec<u8>>, Error>;

    async fn set(&self, key: &str, value: &[u8], ttl: Option<Duration>) -> Result<(), Error>;

    async fn del(&self, key: &str) -> Result<(), Error>;

    /// Conditional write — set the key only if it doesn't exist. Returns
    /// `true` if the value was written, `false` if the key already had
    /// a value.
    async fn set_nx(&self, key: &str, value: &[u8], ttl: Option<Duration>) -> Result<bool, Error>;

    /// Span attribute `cache.system`. `"redis" | "memcached" | "in-memory"`.
    fn system(&self) -> &'static str;
}

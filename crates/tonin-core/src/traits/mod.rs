//! Capability traits services depend on.
//!
//! Services depend on these traits; concrete impls live in their own
//! crates (`tonin-redis`, future `tonin-postgres`, ...). Swapping
//! one impl for another is a `Cargo.toml` change + a `tonin.toml`
//! engine flip, not a handler rewrite. This is the "interface-first"
//! principle from `docs/01-principles.md`.

pub mod cache;
pub mod config;
pub mod database;
pub mod event_bus;
pub mod secret_store;

pub use cache::Cache;
pub use config::{ChainedConfig, Config, EnvConfig, get_typed};
pub use database::{AnyPool, Database};
pub use event_bus::{
    Acker, DeliveredMessage, EventBus, MessageId, StartPosition, SubscribeOptions, Subscription,
};
pub use secret_store::{EnvSecretStore, SecretStore};

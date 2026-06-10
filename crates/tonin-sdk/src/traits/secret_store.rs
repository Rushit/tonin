//! Secret store capability.

use async_trait::async_trait;

use crate::Error;

#[async_trait]
pub trait SecretStore: Send + Sync + 'static {
    /// Resolve a secret by key. Hot-path safe — impls cache and refresh
    /// in the background; this is not expected to hit the provider per
    /// call.
    async fn get(&self, key: &str) -> Result<String, Error>;

    /// Span attribute `secret.provider`.
    /// `"k8s" | "vault" | "external-secrets" | "aws-secrets-manager"`.
    fn provider(&self) -> &'static str;
}

/// Default `SecretStore` impl that reads from the process environment.
/// Works out of the box with the k8s `envFrom: secretRef` wiring the
/// renderer emits — every required key is already an env var by the
/// time the container runs.
pub struct EnvSecretStore;

#[async_trait]
impl SecretStore for EnvSecretStore {
    async fn get(&self, key: &str) -> Result<String, Error> {
        std::env::var(key)
            .map_err(|_| Error::CapabilityPermanent(format!("secret '{key}' not in env")))
    }

    fn provider(&self) -> &'static str {
        "k8s"
    }
}

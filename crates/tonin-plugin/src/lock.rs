use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::Result;
use serde::{Deserialize, Serialize};

/// Desired state for one service in one environment.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ServiceLock {
    pub version: String,
    /// Image pinned by digest: "ghcr.io/org/svc@sha256:...". Mutable tags are rejected by tonin release.
    pub image: String,
    pub git_sha: String,
}

/// The full desired state for one environment — the single source of truth
/// for what should be running. Backed by `environments/<ENV>/lock.toml`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnvironmentLock {
    pub schema: String, // "v1"
    pub env: String,
    pub services: HashMap<String, ServiceLock>,
}

pub type EnvLock = EnvironmentLock;

/// Read/write contract for environment locks. Implement this trait to swap
/// storage backends: git-local (default), S3, Postgres, etc.
pub trait LockStore: Send + Sync {
    fn read(&self, env: &str) -> Result<EnvironmentLock>;
    fn write(&self, lock: &EnvironmentLock) -> Result<()>;
}

/// Default: reads/writes `environments/<ENV>/lock.toml` relative to workspace root.
/// Write is atomic — writes to a `.tmp` file then renames to avoid partial writes.
pub struct GitLockStore {
    pub workspace_root: PathBuf,
}

impl GitLockStore {
    pub fn new(workspace_root: impl AsRef<Path>) -> Self {
        Self {
            workspace_root: workspace_root.as_ref().to_path_buf(),
        }
    }

    fn lock_path(&self, env: &str) -> PathBuf {
        self.workspace_root
            .join("environments")
            .join(env)
            .join("lock.toml")
    }
}

impl LockStore for GitLockStore {
    fn read(&self, env: &str) -> Result<EnvironmentLock> {
        let path = self.lock_path(env);
        let contents = std::fs::read_to_string(&path)
            .map_err(|e| anyhow::anyhow!("reading {}: {e}", path.display()))?;
        toml::from_str(&contents)
            .map_err(|e| anyhow::anyhow!("parsing lock.toml for env={env}: {e}"))
    }

    fn write(&self, lock: &EnvironmentLock) -> Result<()> {
        let path = self.lock_path(&lock.env);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let tmp = path.with_extension("toml.tmp");
        let contents = toml::to_string_pretty(lock)?;
        std::fs::write(&tmp, &contents)?;
        std::fs::rename(&tmp, &path)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_lock(env: &str) -> EnvLock {
        let mut services = HashMap::new();
        services.insert(
            "users-service".into(),
            ServiceLock {
                version: "1.4.0".into(),
                image: "ghcr.io/myorg/users-service@sha256:abc123".into(),
                git_sha: "deadbeef".into(),
            },
        );
        services.insert(
            "orders-service".into(),
            ServiceLock {
                version: "1.2.0".into(),
                image: "ghcr.io/myorg/orders-service@sha256:def456".into(),
                git_sha: "cafebabe".into(),
            },
        );
        EnvLock {
            schema: "v1".into(),
            env: env.into(),
            services,
        }
    }

    #[test]
    fn lock_roundtrip_git_store() {
        let tmp = tempfile::tempdir().unwrap();
        let store = GitLockStore::new(tmp.path());
        let lock = sample_lock("staging");
        store.write(&lock).unwrap();
        let read_back = store.read("staging").unwrap();
        assert_eq!(read_back.services["users-service"].version, "1.4.0");
        assert_eq!(read_back.services["orders-service"].version, "1.2.0");
        assert_eq!(read_back.env, "staging");
        assert_eq!(read_back.schema, "v1");
    }

    #[test]
    fn lock_write_is_atomic() {
        let tmp = tempfile::tempdir().unwrap();
        let store = GitLockStore::new(tmp.path());
        store.write(&sample_lock("dev")).unwrap();
        // tmp file must be gone after successful write
        assert!(!tmp.path().join("environments/dev/lock.toml.tmp").exists());
        assert!(tmp.path().join("environments/dev/lock.toml").exists());
    }

    #[test]
    fn lock_read_missing_env_returns_error() {
        let tmp = tempfile::tempdir().unwrap();
        let store = GitLockStore::new(tmp.path());
        let err = store.read("nonexistent").unwrap_err();
        assert!(
            err.to_string()
                .contains("environments/nonexistent/lock.toml")
        );
    }

    #[test]
    fn lock_write_creates_nested_dirs() {
        let tmp = tempfile::tempdir().unwrap();
        let store = GitLockStore::new(tmp.path());
        // environments/prod/ does not exist yet
        store.write(&sample_lock("prod")).unwrap();
        assert!(tmp.path().join("environments/prod/lock.toml").exists());
    }

    #[test]
    fn lock_update_preserves_other_services() {
        let tmp = tempfile::tempdir().unwrap();
        let store = GitLockStore::new(tmp.path());
        store.write(&sample_lock("staging")).unwrap();

        // Update just users-service, orders-service should be unchanged
        let mut updated = store.read("staging").unwrap();
        updated.services.get_mut("users-service").unwrap().version = "2.0.0".into();
        store.write(&updated).unwrap();

        let final_lock = store.read("staging").unwrap();
        assert_eq!(final_lock.services["users-service"].version, "2.0.0");
        assert_eq!(final_lock.services["orders-service"].version, "1.2.0"); // unchanged
    }
}

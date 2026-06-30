use std::path::PathBuf;

use anyhow::Result;
use clap::Args;
use tonin_plugin::lock::{EnvLock, GitLockStore, LockStore, ServiceLock};

#[derive(Args)]
pub struct ReleaseArgs {
    /// Service name (e.g. "users-service").
    #[arg(long)]
    pub service: String,

    /// New version string (e.g. "1.4.0").
    #[arg(long)]
    pub version: String,

    /// Image reference with digest (e.g. "ghcr.io/org/svc@sha256:...").
    /// Mutable tags (without @sha256:) are rejected.
    #[arg(long, required_unless_present = "digest")]
    pub image: Option<String>,

    /// Git SHA of the commit that produced this release.
    #[arg(long)]
    pub git_sha: Option<String>,

    /// Image digest (e.g. "sha256:...").
    #[arg(long, required_unless_present = "image")]
    pub digest: Option<String>,

    /// Registry override (e.g. "ghcr.io/myorg").
    /// Used only when constructing image ref from digest (when --image not provided).
    #[arg(long)]
    pub registry: Option<String>,

    /// Environment lock to update (default: staging).
    #[arg(long, default_value = "staging")]
    pub env: String,

    /// Workspace root containing environments/.
    #[arg(long)]
    pub workspace: Option<PathBuf>,
}

pub fn run(args: ReleaseArgs) -> Result<()> {
    let digest_val = args.digest.as_deref().unwrap_or("");
    let full_digest = if digest_val.is_empty() {
        "".to_string()
    } else if digest_val.starts_with("sha256:") {
        digest_val.to_string()
    } else {
        format!("sha256:{}", digest_val)
    };

    let image_ref = match &args.image {
        Some(img) => {
            anyhow::ensure!(
                img.contains("@sha256:"),
                "image must be pinned by digest (contain '@sha256:'), got: {}",
                img
            );
            img.clone()
        }
        None => {
            anyhow::ensure!(
                !full_digest.is_empty(),
                "either --image or --digest must be provided"
            );
            let registry = args.registry.as_deref().unwrap_or("ghcr.io/myorg");
            format!("{}/{}@{}", registry, args.service, full_digest)
        }
    };

    let git_sha_ref = args.git_sha.clone().unwrap_or_else(|| "HEAD".to_string());

    let root = args
        .workspace
        .unwrap_or_else(|| std::env::current_dir().unwrap());
    let store = GitLockStore::new(&root);

    let mut lock = store.read(&args.env).unwrap_or_else(|_| EnvLock {
        schema: "v1".into(),
        env: args.env.clone(),
        services: Default::default(),
    });

    let prev = lock.services.get(&args.service).map(|s| s.version.clone());
    lock.services.insert(
        args.service.clone(),
        ServiceLock {
            version: args.version.clone(),
            image: image_ref,
            git_sha: git_sha_ref,
        },
    );
    store.write(&lock)?;

    match prev {
        Some(v) => println!(
            "✓ environments/{}/lock.toml: {} {} → {}",
            args.env, args.service, v, args.version
        ),
        None => println!(
            "✓ environments/{}/lock.toml: {} (new) → {}",
            args.env, args.service, args.version
        ),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn release_creates_lock_entry_with_digest() {
        let tmp = tempfile::tempdir().unwrap();
        let store = GitLockStore::new(tmp.path());

        run(ReleaseArgs {
            service: "users-service".into(),
            version: "1.4.0".into(),
            image: None,
            git_sha: None,
            digest: Some("abc123456789".into()),
            registry: None,
            env: "staging".into(),
            workspace: Some(tmp.path().to_path_buf()),
        })
        .unwrap();

        let lock = store.read("staging").unwrap();
        assert_eq!(lock.services["users-service"].version, "1.4.0");
        assert_eq!(
            lock.services["users-service"].image,
            "ghcr.io/myorg/users-service@sha256:abc123456789"
        );
        assert_eq!(lock.services["users-service"].git_sha, "HEAD");
    }

    #[test]
    fn release_rejects_mutable_tag() {
        let tmp = tempfile::tempdir().unwrap();

        let result = run(ReleaseArgs {
            service: "orders-service".into(),
            version: "1.2.0".into(),
            image: Some("ghcr.io/myorg/orders-service:1.2.0".into()),
            git_sha: Some("deadbeef".into()),
            digest: None,
            registry: None,
            env: "staging".into(),
            workspace: Some(tmp.path().to_path_buf()),
        });

        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("digest"));
    }
}

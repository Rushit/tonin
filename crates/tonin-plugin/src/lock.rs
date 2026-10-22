use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::Result;
use serde::{Deserialize, Serialize, Serializer};
use toml_edit::{DocumentMut, Item, Table, TableLike};

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
///
/// Services always serialize sorted by name, so the output does not depend on
/// map iteration order.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EnvironmentLock {
    pub schema: String, // "v1"
    pub env: String,
    #[serde(default, serialize_with = "serialize_sorted")]
    pub services: HashMap<String, ServiceLock>,
}

pub type EnvLock = EnvironmentLock;

fn serialize_sorted<S: Serializer>(
    services: &HashMap<String, ServiceLock>,
    serializer: S,
) -> std::result::Result<S::Ok, S::Error> {
    services
        .iter()
        .collect::<BTreeMap<_, _>>()
        .serialize(serializer)
}

/// What a lock write did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum WriteOutcome {
    /// The stored lock changed.
    Written,
    /// The stored lock already held this exact state; nothing was rewritten.
    Unchanged,
}

impl WriteOutcome {
    pub fn is_unchanged(self) -> bool {
        self == WriteOutcome::Unchanged
    }
}

/// Read/write contract for environment locks. Implement this trait to swap
/// storage backends: git-local (default), S3, Postgres, etc.
pub trait LockStore: Send + Sync {
    fn read(&self, env: &str) -> Result<EnvironmentLock>;
    fn write(&self, lock: &EnvironmentLock) -> Result<()>;

    /// Write `lock` only when it differs from the stored state, and report
    /// whether anything changed (callers can skip committing when it didn't).
    ///
    /// The default implementation compares against [`LockStore::read`]; if
    /// that read fails (no lock yet, or one that can't be read), the lock is
    /// written anyway and the outcome is [`WriteOutcome::Written`].
    fn write_if_changed(&self, lock: &EnvironmentLock) -> Result<WriteOutcome> {
        if self.read(&lock.env).ok().as_ref() == Some(lock) {
            return Ok(WriteOutcome::Unchanged);
        }
        self.write(lock)?;
        Ok(WriteOutcome::Written)
    }
}

/// Default: reads/writes `environments/<ENV>/lock.toml` relative to workspace root.
/// Write is atomic — writes to a uniquely named `.tmp` file then renames to
/// avoid partial writes.
///
/// Writes are minimal: an existing file is edited in place (comments and
/// formatting kept, only changed entries touched), `[services.*]` tables are
/// kept sorted by name, and a file that already holds the same state is left
/// untouched. Only `[services.<name>]` tables are supported as a layout: other
/// spellings (dotted keys, inline tables) are normalised to that form the next
/// time the file is written.
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

    /// Read the lock for `env`, or `None` when no lock file exists yet. Any
    /// other failure (I/O error, invalid TOML, a malformed entry) is an error.
    pub fn read_if_exists(&self, env: &str) -> Result<Option<EnvironmentLock>> {
        let path = self.lock_path(env);
        let contents = match std::fs::read_to_string(&path) {
            Ok(s) => s,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => anyhow::bail!("reading {}: {e}", path.display()),
        };
        toml::from_str(&contents)
            .map(Some)
            .map_err(|e| anyhow::anyhow!("parsing {}: {e}", path.display()))
    }
}

static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// A temp path next to `path`, unique per process and per write.
fn tmp_path(path: &Path) -> PathBuf {
    let n = TMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(format!(".{}.{n}.tmp", std::process::id()));
    path.with_file_name(name)
}

impl LockStore for GitLockStore {
    fn read(&self, env: &str) -> Result<EnvironmentLock> {
        self.read_if_exists(env)?.ok_or_else(|| {
            anyhow::anyhow!("reading {}: no such file", self.lock_path(env).display())
        })
    }

    fn write(&self, lock: &EnvironmentLock) -> Result<()> {
        self.write_if_changed(lock).map(|_| ())
    }

    fn write_if_changed(&self, lock: &EnvironmentLock) -> Result<WriteOutcome> {
        let path = self.lock_path(&lock.env);
        let existing = match std::fs::read_to_string(&path) {
            Ok(s) => Some(s),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => anyhow::bail!("reading {}: {e}", path.display()),
        };
        if let Some(current) = &existing
            && toml::from_str::<EnvironmentLock>(current).ok().as_ref() == Some(lock)
        {
            return Ok(WriteOutcome::Unchanged);
        }
        let contents = render_lock(existing.as_deref(), lock);
        if existing.as_deref() == Some(contents.as_str()) {
            return Ok(WriteOutcome::Unchanged);
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let tmp = tmp_path(&path);
        std::fs::write(&tmp, &contents)?;
        if let Err(e) = std::fs::rename(&tmp, &path) {
            let _ = std::fs::remove_file(&tmp);
            return Err(e.into());
        }
        Ok(WriteOutcome::Written)
    }
}

/// Render `lock` as TOML. When `existing` parses, it is edited in place so
/// unchanged entries keep their exact bytes; `[services.*]` tables are ordered
/// by service name.
fn render_lock(existing: Option<&str>, lock: &EnvironmentLock) -> String {
    let mut doc = existing
        .and_then(|s| s.parse::<DocumentMut>().ok())
        .unwrap_or_default();
    let root = doc.as_table_mut();
    set_str(root, "schema", &lock.schema);
    set_str(root, "env", &lock.env);

    if normalise_table(root.entry("services").or_insert(Item::None), true)
        && let Some(mut key) = root.key_mut("services")
    {
        key.fmt();
    }
    let services = root
        .get_mut("services")
        .and_then(Item::as_table_mut)
        .expect("services is a table");

    let stale: Vec<String> = services
        .iter()
        .map(|(name, _)| name.to_string())
        .filter(|name| !lock.services.contains_key(name))
        .collect();
    for name in stale {
        services.remove(&name);
    }

    for (name, svc) in &lock.services {
        if normalise_table(services.entry(name).or_insert(Item::None), false)
            && let Some(mut key) = services.key_mut(name)
        {
            key.fmt();
        }
        let table = services
            .get_mut(name)
            .and_then(Item::as_table_mut)
            .expect("service entry is a table");
        set_str(table, "version", &svc.version);
        set_str(table, "image", &svc.image);
        set_str(table, "git_sha", &svc.git_sha);
    }

    // Order by name: sort the keys, then give every service table the same
    // document position so the (stable) emit order follows the sorted keys.
    services.sort_values();
    let position = services
        .iter()
        .filter_map(|(_, item)| item.as_table().and_then(Table::position))
        .min();
    for (_, item) in services.iter_mut() {
        if let Some(table) = item.as_table_mut() {
            table.set_position(position);
        }
    }
    if services.is_empty() {
        services.set_implicit(false);
    }

    doc.to_string()
}

/// Make `item` a standard `[header]` table: inline tables and dotted-key
/// tables are converted, anything else that isn't a table is replaced by an
/// empty one. A standard table is left untouched (its bytes are kept).
/// Returns whether `item` was changed.
fn normalise_table(item: &mut Item, implicit: bool) -> bool {
    if matches!(item, Item::Table(t) if !t.is_dotted()) {
        return false;
    }
    let mut table = std::mem::take(item).into_table().unwrap_or_default();
    table.set_dotted(false);
    table.set_implicit(implicit);
    *item = Item::Table(table);
    true
}

/// Set a string key, leaving it (and its formatting) alone when already equal.
fn set_str(table: &mut dyn TableLike, key: &str, value: &str) {
    if table.get(key).and_then(Item::as_str) != Some(value) {
        table.insert(key, toml_edit::value(value));
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
        // no tmp file may be left behind after a successful write
        let names: Vec<String> = std::fs::read_dir(tmp.path().join("environments/dev"))
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, ["lock.toml"]);
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

    fn svc(version: &str, image: &str, git_sha: &str) -> ServiceLock {
        ServiceLock {
            version: version.into(),
            image: image.into(),
            git_sha: git_sha.into(),
        }
    }

    fn lock_with(env: &str, services: &[(&str, ServiceLock)]) -> EnvLock {
        EnvLock {
            schema: "v1".into(),
            env: env.into(),
            services: services
                .iter()
                .map(|(name, s)| (name.to_string(), s.clone()))
                .collect(),
        }
    }

    fn lock_file(root: &Path, env: &str) -> PathBuf {
        root.join("environments").join(env).join("lock.toml")
    }

    const GOLDEN: &str = r#"schema = "v1"
env = "prd"

[services.alpha]
version = "1.0.0"
image = "ghcr.io/acme/alpha@sha256:aaa"
git_sha = "a1"

[services.bravo]
version = "2.0.0"
image = "ghcr.io/acme/bravo@sha256:bbb"
git_sha = "b2"

[services.charlie]
version = "3.0.0"
image = "ghcr.io/acme/charlie@sha256:ccc"
git_sha = "c3"
"#;

    fn golden_lock() -> EnvLock {
        lock_with(
            "prd",
            &[
                (
                    "charlie",
                    svc("3.0.0", "ghcr.io/acme/charlie@sha256:ccc", "c3"),
                ),
                ("alpha", svc("1.0.0", "ghcr.io/acme/alpha@sha256:aaa", "a1")),
                ("bravo", svc("2.0.0", "ghcr.io/acme/bravo@sha256:bbb", "b2")),
            ],
        )
    }

    #[test]
    fn fresh_write_matches_golden() {
        let tmp = tempfile::tempdir().unwrap();
        let store = GitLockStore::new(tmp.path());
        let outcome = store.write_if_changed(&golden_lock()).unwrap();
        assert_eq!(outcome, WriteOutcome::Written);
        let written = std::fs::read_to_string(lock_file(tmp.path(), "prd")).unwrap();
        assert_eq!(written, GOLDEN);
    }

    #[test]
    fn serde_serialization_is_sorted() {
        let out = toml::to_string_pretty(&golden_lock()).unwrap();
        let a = out.find("[services.alpha]").unwrap();
        let b = out.find("[services.bravo]").unwrap();
        let c = out.find("[services.charlie]").unwrap();
        assert!(a < b && b < c, "{out}");
    }

    #[test]
    fn order_is_stable_regardless_of_insertion_order() {
        let entries = [
            (
                "charlie",
                svc("3.0.0", "ghcr.io/acme/charlie@sha256:ccc", "c3"),
            ),
            ("alpha", svc("1.0.0", "ghcr.io/acme/alpha@sha256:aaa", "a1")),
            ("bravo", svc("2.0.0", "ghcr.io/acme/bravo@sha256:bbb", "b2")),
        ];
        let orders: [[usize; 3]; 6] = [
            [0, 1, 2],
            [0, 2, 1],
            [1, 0, 2],
            [1, 2, 0],
            [2, 0, 1],
            [2, 1, 0],
        ];
        for order in orders {
            // Build up the lock one release at a time, in this order.
            let tmp = tempfile::tempdir().unwrap();
            let store = GitLockStore::new(tmp.path());
            let mut lock = lock_with("prd", &[]);
            for i in order {
                let (name, s) = &entries[i];
                lock.services.insert(name.to_string(), s.clone());
                store.write(&lock).unwrap();
            }
            let written = std::fs::read_to_string(lock_file(tmp.path(), "prd")).unwrap();
            assert_eq!(written, GOLDEN, "insertion order {order:?}");
        }
    }

    #[test]
    fn unchanged_write_is_noop() {
        let tmp = tempfile::tempdir().unwrap();
        let store = GitLockStore::new(tmp.path());
        let path = lock_file(tmp.path(), "prd");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        // Hand-formatted, unsorted, commented: semantically equal to the lock.
        let original = "# managed by release\nschema = \"v1\"\nenv = \"prd\"\n\n\
            [services.charlie]\nversion = \"3.0.0\"\nimage = \"ghcr.io/acme/charlie@sha256:ccc\"\ngit_sha = \"c3\"\n\n\
            [services.alpha]\nversion=\"1.0.0\"\nimage = \"ghcr.io/acme/alpha@sha256:aaa\"\ngit_sha = \"a1\"\n\n\
            [services.bravo]\nversion = \"2.0.0\"\nimage = \"ghcr.io/acme/bravo@sha256:bbb\"\ngit_sha = \"b2\"\n";
        std::fs::write(&path, original).unwrap();
        let before = std::fs::metadata(&path).unwrap().modified().unwrap();

        let outcome = store.write_if_changed(&golden_lock()).unwrap();
        assert_eq!(outcome, WriteOutcome::Unchanged);
        assert!(outcome.is_unchanged());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
        assert_eq!(
            std::fs::metadata(&path).unwrap().modified().unwrap(),
            before
        );

        // Re-writing the golden file with its own content is byte-identical too.
        std::fs::write(&path, GOLDEN).unwrap();
        store.write(&store.read("prd").unwrap()).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), GOLDEN);
    }

    #[test]
    fn updating_one_service_only_changes_its_table() {
        let tmp = tempfile::tempdir().unwrap();
        let store = GitLockStore::new(tmp.path());
        let path = lock_file(tmp.path(), "prd");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, GOLDEN).unwrap();

        let mut lock = store.read("prd").unwrap();
        lock.services.insert(
            "bravo".into(),
            svc("2.1.0", "ghcr.io/acme/bravo@sha256:bbb2", "b2"),
        );
        assert_eq!(
            store.write_if_changed(&lock).unwrap(),
            WriteOutcome::Written
        );

        let expected = GOLDEN
            .replace("version = \"2.0.0\"", "version = \"2.1.0\"")
            .replace("bravo@sha256:bbb\"", "bravo@sha256:bbb2\"");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), expected);
    }

    #[test]
    fn update_preserves_comments_and_formatting_of_other_tables() {
        let tmp = tempfile::tempdir().unwrap();
        let store = GitLockStore::new(tmp.path());
        let path = lock_file(tmp.path(), "prd");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let original = "# managed by release\nschema = \"v1\"\nenv = \"prd\"\n\n\
            [services.alpha]\nversion=\"1.0.0\"  # pinned\nimage = \"ghcr.io/acme/alpha@sha256:aaa\"\ngit_sha = \"a1\"\n\n\
            [services.bravo]\nversion = \"2.0.0\"\nimage = \"ghcr.io/acme/bravo@sha256:bbb\"\ngit_sha = \"b2\"\n";
        std::fs::write(&path, original).unwrap();

        let mut lock = store.read("prd").unwrap();
        lock.services.get_mut("bravo").unwrap().git_sha = "b3".into();
        store.write(&lock).unwrap();

        let expected = original.replace("git_sha = \"b2\"", "git_sha = \"b3\"");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), expected);
    }

    #[test]
    fn new_service_is_inserted_in_sorted_position() {
        let tmp = tempfile::tempdir().unwrap();
        let store = GitLockStore::new(tmp.path());
        let path = lock_file(tmp.path(), "prd");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let without_bravo = GOLDEN.replace(
            "[services.bravo]\nversion = \"2.0.0\"\nimage = \"ghcr.io/acme/bravo@sha256:bbb\"\ngit_sha = \"b2\"\n\n",
            "",
        );
        assert_ne!(without_bravo, GOLDEN);
        std::fs::write(&path, &without_bravo).unwrap();

        let mut lock = store.read("prd").unwrap();
        lock.services.insert(
            "bravo".into(),
            svc("2.0.0", "ghcr.io/acme/bravo@sha256:bbb", "b2"),
        );
        store.write(&lock).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), GOLDEN);
    }

    #[test]
    fn unsorted_file_is_sorted_when_it_changes() {
        let tmp = tempfile::tempdir().unwrap();
        let store = GitLockStore::new(tmp.path());
        let path = lock_file(tmp.path(), "prd");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        // charlie moved to the top, as an unstable writer would leave it.
        let unsorted = "schema = \"v1\"\nenv = \"prd\"\n\n\
            [services.charlie]\nversion = \"3.0.0\"\nimage = \"ghcr.io/acme/charlie@sha256:ccc\"\ngit_sha = \"c3\"\n\n\
            [services.alpha]\nversion = \"1.0.0\"\nimage = \"ghcr.io/acme/alpha@sha256:aaa\"\ngit_sha = \"a1\"\n\n\
            [services.bravo]\nversion = \"2.0.0\"\nimage = \"ghcr.io/acme/bravo@sha256:bbb\"\ngit_sha = \"b2\"\n";
        std::fs::write(&path, unsorted).unwrap();

        let mut lock = store.read("prd").unwrap();
        lock.services.get_mut("alpha").unwrap().version = "1.0.1".into();
        store.write(&lock).unwrap();
        let expected = GOLDEN.replace("version = \"1.0.0\"", "version = \"1.0.1\"");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), expected);
    }

    #[test]
    fn removed_service_drops_its_table() {
        let tmp = tempfile::tempdir().unwrap();
        let store = GitLockStore::new(tmp.path());
        let path = lock_file(tmp.path(), "prd");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, GOLDEN).unwrap();

        let mut lock = store.read("prd").unwrap();
        lock.services.remove("bravo");
        store.write(&lock).unwrap();
        let read_back = store.read("prd").unwrap();
        assert_eq!(read_back, lock);
        assert!(!std::fs::read_to_string(&path).unwrap().contains("bravo"));
    }

    #[test]
    fn empty_lock_roundtrips() {
        let tmp = tempfile::tempdir().unwrap();
        let store = GitLockStore::new(tmp.path());
        let lock = lock_with("dev", &[]);
        assert_eq!(
            store.write_if_changed(&lock).unwrap(),
            WriteOutcome::Written
        );
        assert_eq!(store.read("dev").unwrap(), lock);
        assert_eq!(
            store.write_if_changed(&lock).unwrap(),
            WriteOutcome::Unchanged
        );
    }

    /// A store that only implements `read`/`write`, to exercise the default
    /// `write_if_changed`.
    #[derive(Default)]
    struct MemStore {
        lock: std::sync::Mutex<Option<EnvLock>>,
        writes: std::sync::atomic::AtomicUsize,
        fail_reads: bool,
    }

    impl LockStore for MemStore {
        fn read(&self, env: &str) -> Result<EnvironmentLock> {
            anyhow::ensure!(!self.fail_reads, "backend unavailable");
            self.lock
                .lock()
                .unwrap()
                .clone()
                .filter(|l| l.env == env)
                .ok_or_else(|| anyhow::anyhow!("no lock for {env}"))
        }

        fn write(&self, lock: &EnvironmentLock) -> Result<()> {
            self.writes.fetch_add(1, Ordering::SeqCst);
            *self.lock.lock().unwrap() = Some(lock.clone());
            Ok(())
        }
    }

    #[test]
    fn default_write_if_changed_skips_identical_and_writes_changes() {
        let store = MemStore::default();
        let mut lock = golden_lock();
        // nothing stored yet: read fails → written
        assert_eq!(
            store.write_if_changed(&lock).unwrap(),
            WriteOutcome::Written
        );
        assert_eq!(store.writes.load(Ordering::SeqCst), 1);
        // identical → unchanged, no write
        assert_eq!(
            store.write_if_changed(&lock).unwrap(),
            WriteOutcome::Unchanged
        );
        assert_eq!(store.writes.load(Ordering::SeqCst), 1);
        // changed → written
        lock.services.get_mut("alpha").unwrap().version = "1.0.1".into();
        assert_eq!(
            store.write_if_changed(&lock).unwrap(),
            WriteOutcome::Written
        );
        assert_eq!(store.writes.load(Ordering::SeqCst), 2);
        assert_eq!(store.read("prd").unwrap(), lock);
    }

    #[test]
    fn default_write_if_changed_writes_when_read_fails() {
        let store = MemStore {
            fail_reads: true,
            ..Default::default()
        };
        let lock = golden_lock();
        *store.lock.lock().unwrap() = Some(lock.clone());
        assert_eq!(
            store.write_if_changed(&lock).unwrap(),
            WriteOutcome::Written
        );
        assert_eq!(store.writes.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn read_if_exists_distinguishes_missing_from_malformed() {
        let tmp = tempfile::tempdir().unwrap();
        let store = GitLockStore::new(tmp.path());
        assert!(store.read_if_exists("prd").unwrap().is_none());

        let path = lock_file(tmp.path(), "prd");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        // valid TOML, but bravo is missing git_sha
        let malformed = GOLDEN.replace("git_sha = \"b2\"\n", "");
        std::fs::write(&path, &malformed).unwrap();
        let err = store.read_if_exists("prd").unwrap_err().to_string();
        assert!(err.contains("environments/prd/lock.toml"), "{err}");
        assert!(err.contains("git_sha"), "{err}");
    }

    #[test]
    fn dotted_and_inline_layouts_are_normalised_on_write() {
        let tmp = tempfile::tempdir().unwrap();
        let store = GitLockStore::new(tmp.path());
        let path = lock_file(tmp.path(), "prd");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let odd = "schema = \"v1\"\nenv = \"prd\"\n\
            services.charlie = { version = \"3.0.0\", image = \"ghcr.io/acme/charlie@sha256:ccc\", git_sha = \"c3\" }\n\
            services.alpha.version = \"1.0.0\"\n\
            services.alpha.image = \"ghcr.io/acme/alpha@sha256:aaa\"\n\
            services.alpha.git_sha = \"a1\"\n";
        std::fs::write(&path, odd).unwrap();

        // Unchanged content is still left alone.
        let mut lock = store.read("prd").unwrap();
        assert!(store.write_if_changed(&lock).unwrap().is_unchanged());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), odd);

        lock.services.insert(
            "bravo".into(),
            svc("2.0.0", "ghcr.io/acme/bravo@sha256:bbb", "b2"),
        );
        store.write(&lock).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), GOLDEN);
    }
}

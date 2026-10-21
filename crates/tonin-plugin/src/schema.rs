//! Strict `tonin.toml` schema: every key must be known.
//!
//! serde alone cannot reject unknown keys in the sections that take
//! per-environment overlays (`[deploy.<env>]`, `[database.<env>]`, …), since
//! any table key there may be an environment name. This module walks the
//! parsed document (with spans) against a static description of the schema,
//! before serde runs, and reports the first unknown key with its line, the
//! table it is in and the keys allowed there. Value types are still checked
//! by serde and the section resolvers.

use std::ops::Range;

use toml_edit::{ImDocument, Item, TableLike};

/// What a key's value may be.
#[derive(Clone, Copy)]
pub(crate) enum Shape {
    /// A value whose type serde (or the section's resolver) checks.
    Value,
    /// A free-form table passed through to Kubernetes (e.g. `[security.pod]`).
    Free,
    /// A table with known keys.
    Table(&'static TableSchema),
    /// An array of tables with known keys, as `[[volumes]]` or as an inline
    /// array of inline tables (`fqdns = [{ name = "…" }]`).
    Array(&'static TableSchema),
    /// `[callers]`: entries are a namespace string, a caller table
    /// (`namespace`, `labels`, `service`, `ports`), or an env overlay (a
    /// table with none of those keys) holding entries of the first two forms.
    Callers,
    /// A table whose keys are names chosen by the user, each a table of this
    /// schema (`[databases.<name>]`, `[client.cache.<method>]`).
    Named(&'static TableSchema),
    /// A table whose keys are names chosen by the user, each either a value
    /// or a table of this schema (`[depends_on]`, `[callers]`).
    NamedOr(&'static TableSchema),
}

/// The keys of one table.
pub(crate) struct TableSchema {
    pub fields: &'static [(&'static str, Shape)],
    /// Per-environment overlays: any other key holding a table is an
    /// environment name, its table checked against this schema.
    pub overlay: Option<&'static TableSchema>,
    /// Any other key holding a plain value is accepted (e.g. `[env]` vars or
    /// per-env namespace overrides in `[depends_on]`).
    pub open_values: bool,
    /// Those other plain keys are environment names (`prd = "ns"`).
    pub open_envs: bool,
}

/// An environment named by an overlay table or override key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OverlayEnv {
    pub env: String,
    /// Where it is named (`[deploy.prd]`, `depends_on.identity.prd`).
    pub location: String,
    pub line: usize,
}

/// An unknown key (or a misplaced value) in `tonin.toml`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SchemaError {
    pub line: usize,
    pub column: usize,
    /// Dotted path of the offending key (`deploy.replica`).
    pub key: String,
    /// The table it is in (`[deploy]`, `[[volumes]]`, `[deploy.prd]`).
    pub table: String,
    pub reason: String,
    /// Keys allowed in that table.
    pub allowed: Vec<String>,
}

impl std::fmt::Display for SchemaError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "line {}, column {}: {} `{}` in {}",
            self.line, self.column, self.reason, self.key, self.table
        )?;
        if !self.allowed.is_empty() {
            write!(f, "; allowed keys: {}", self.allowed.join(", "))?;
        }
        Ok(())
    }
}

struct Walker<'a> {
    src: &'a str,
    envs: std::cell::RefCell<Vec<OverlayEnv>>,
}

/// An environment name: lowercase letters, digits and '-'.
fn is_env_name(s: &str) -> bool {
    !s.is_empty()
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && !s.starts_with('-')
        && !s.ends_with('-')
}

impl Walker<'_> {
    fn position(&self, span: Option<Range<usize>>) -> (usize, usize) {
        let Some(span) = span else {
            return (0, 0);
        };
        let before = &self.src[..span.start.min(self.src.len())];
        let line = before.matches('\n').count() + 1;
        let column = before.rsplit('\n').next().map_or(0, |l| l.chars().count()) + 1;
        (line, column)
    }

    fn error(
        &self,
        span: Option<Range<usize>>,
        key: String,
        table: &str,
        reason: &str,
        schema: Option<&TableSchema>,
    ) -> SchemaError {
        let (line, column) = self.position(span);
        let mut allowed: Vec<String> = schema
            .map(|s| s.fields.iter().map(|(k, _)| k.to_string()).collect())
            .unwrap_or_default();
        if let Some(s) = schema
            && s.overlay.is_some()
        {
            let base = table.trim_start_matches('[').trim_end_matches(']');
            allowed.push(format!("[{base}.<env>] overlays"));
        }
        SchemaError {
            line,
            column,
            key,
            table: table.to_string(),
            reason: reason.to_string(),
            allowed,
        }
    }

    fn table(
        &self,
        t: &dyn TableLike,
        schema: &TableSchema,
        path: &str,
        table: &str,
    ) -> Result<(), SchemaError> {
        for (key, item) in t.iter() {
            let key_span = t.key(key).and_then(|k| k.span());
            let full = join(path, key);
            if let Some((_, shape)) = schema.fields.iter().find(|(k, _)| *k == key) {
                self.shape(item, *shape, &full, key_span)?;
                continue;
            }
            let is_table = item.as_table_like().is_some();
            match (is_table, schema.overlay) {
                (true, Some(overlay)) => {
                    self.env_name(
                        key,
                        key_span.clone(),
                        &full,
                        &format!("[{full}]"),
                        schema,
                        table,
                    )?;
                    let t = item.as_table_like().expect("checked");
                    self.table(t, overlay, &full, &format!("[{full}]"))?;
                }
                (false, _) if schema.open_values => {
                    if schema.open_envs {
                        self.env_name(key, key_span.clone(), &full, &full, schema, table)?;
                    }
                }
                _ => {
                    return Err(self.error(key_span, full, table, "unknown key", Some(schema)));
                }
            }
        }
        Ok(())
    }

    /// Record an environment named by an overlay; reject a name that cannot
    /// be one (most likely a misspelt key).
    fn env_name(
        &self,
        env: &str,
        span: Option<Range<usize>>,
        full: &str,
        location: &str,
        schema: &TableSchema,
        table: &str,
    ) -> Result<(), SchemaError> {
        if !is_env_name(env) {
            return Err(self.error(
                span,
                full.to_string(),
                table,
                "unknown key (not an environment name either)",
                Some(schema),
            ));
        }
        let (line, _) = self.position(span);
        self.envs.borrow_mut().push(OverlayEnv {
            env: env.to_string(),
            location: location.to_string(),
            line,
        });
        Ok(())
    }

    fn shape(
        &self,
        item: &Item,
        shape: Shape,
        path: &str,
        key_span: Option<Range<usize>>,
    ) -> Result<(), SchemaError> {
        let expect_table = |what: &str| {
            self.error(
                key_span.clone(),
                path.to_string(),
                &parent_table(path),
                &format!("expected {what} for"),
                None,
            )
        };
        match shape {
            Shape::Value | Shape::Free => Ok(()),
            Shape::Table(schema) => {
                let t = item
                    .as_table_like()
                    .ok_or_else(|| expect_table("a table"))?;
                self.table(t, schema, path, &format!("[{path}]"))
            }
            Shape::Array(schema) => {
                if let Some(arr) = item.as_array_of_tables() {
                    for (i, t) in arr.iter().enumerate() {
                        self.table(t, schema, &format!("{path}[{i}]"), &format!("[[{path}]]"))?;
                    }
                    return Ok(());
                }
                let arr = item
                    .as_array()
                    .ok_or_else(|| expect_table("an array of tables"))?;
                for (i, v) in arr.iter().enumerate() {
                    let t = v.as_inline_table().ok_or_else(|| {
                        self.error(
                            v.span(),
                            format!("{path}[{i}]"),
                            &parent_table(path),
                            "expected a table for",
                            None,
                        )
                    })?;
                    let at = format!("{path}[{i}]");
                    self.table(t, schema, &at, &at)?;
                }
                Ok(())
            }
            Shape::Callers => {
                let t = item
                    .as_table_like()
                    .ok_or_else(|| expect_table("a table"))?;
                for (name, entry) in t.iter() {
                    let full = join(path, name);
                    let Some(e) = entry.as_table_like() else {
                        continue; // `name = "<namespace>"`
                    };
                    let is_caller = e
                        .iter()
                        .any(|(k, _)| CALLER.fields.iter().any(|(f, _)| *f == k));
                    if is_caller || e.is_empty() {
                        self.table(e, &CALLER, &full, &format!("[{full}]"))?;
                        continue;
                    }
                    // `[callers.<env>]` overlay: entries of the same two forms.
                    let span = t.key(name).and_then(|k| k.span());
                    self.env_name(
                        name,
                        span,
                        &full,
                        &format!("[{full}]"),
                        &CALLER,
                        &format!("[{path}]"),
                    )?;
                    for (inner, v) in e.iter() {
                        if let Some(c) = v.as_table_like() {
                            let f = join(&full, inner);
                            self.table(c, &CALLER, &f, &format!("[{f}]"))?;
                        }
                    }
                }
                Ok(())
            }
            Shape::Named(schema) | Shape::NamedOr(schema) => {
                let t = item
                    .as_table_like()
                    .ok_or_else(|| expect_table("a table"))?;
                for (name, entry) in t.iter() {
                    let full = join(path, name);
                    match entry.as_table_like() {
                        Some(e) => self.table(e, schema, &full, &format!("[{full}]"))?,
                        None if matches!(shape, Shape::NamedOr(_)) => {}
                        None => {
                            let span = t.key(name).and_then(|k| k.span());
                            return Err(self.error(
                                span,
                                full,
                                &format!("[{path}]"),
                                "expected a table for",
                                None,
                            ));
                        }
                    }
                }
                Ok(())
            }
        }
    }
}

fn join(path: &str, key: &str) -> String {
    if path.is_empty() {
        key.to_string()
    } else {
        format!("{path}.{key}")
    }
}

fn parent_table(path: &str) -> String {
    match path.rsplit_once('.') {
        Some((parent, _)) => format!("[{parent}]"),
        None => "the top level".to_string(),
    }
}

/// Check `src` against `root`. TOML syntax errors are left to serde, which
/// reports them with their position.
pub(crate) fn validate(src: &str, root: &TableSchema) -> Result<Vec<OverlayEnv>, SchemaError> {
    let Ok(doc) = ImDocument::parse(src) else {
        return Ok(Vec::new());
    };
    let walker = Walker {
        src,
        envs: Default::default(),
    };
    walker.table(doc.as_table(), root, "", "the top level")?;
    Ok(walker.envs.into_inner())
}

/// The environments a `tonin.toml` names in overlays (`[deploy.prd]`,
/// `[env.dev]`, `[callers.prd]`, `depends_on.x.prd = "…"`, …), after
/// checking it against the schema. `tonin helm generate` warns about the
/// ones it is not rendering: an overlay for a misspelt environment would
/// otherwise never apply.
pub fn overlay_envs(src: &str) -> Result<Vec<OverlayEnv>, SchemaError> {
    validate(src, &TONIN_TOML)
}

// ---------- the tonin.toml schema ----------

const fn table(fields: &'static [(&'static str, Shape)]) -> TableSchema {
    TableSchema {
        fields,
        overlay: None,
        open_values: false,
        open_envs: false,
    }
}

const fn with_overlay(
    fields: &'static [(&'static str, Shape)],
    overlay: &'static TableSchema,
) -> TableSchema {
    TableSchema {
        fields,
        overlay: Some(overlay),
        open_values: false,
        open_envs: false,
    }
}

static IMAGE: TableSchema = table(&[("registry", Shape::Value)]);
static SECURITY: TableSchema = table(&[("pod", Shape::Free), ("container", Shape::Free)]);

static STARTUP: TableSchema = table(&[
    ("path", Shape::Value),
    ("period_seconds", Shape::Value),
    ("failure_threshold", Shape::Value),
]);
static HEALTH: TableSchema = table(&[
    ("path", Shape::Value),
    ("port", Shape::Value),
    ("liveness_path", Shape::Value),
    ("readiness_path", Shape::Value),
    ("startup", Shape::Table(&STARTUP)),
]);
static HTTP: TableSchema = table(&[("port", Shape::Value), ("health_path", Shape::Value)]);
static SERVICE: TableSchema = table(&[
    ("name", Shape::Value),
    ("version", Shape::Value),
    ("language", Shape::Value),
    ("type", Shape::Value),
    ("web_mode", Shape::Value),
    ("codec", Shape::Value),
    ("port", Shape::Value),
    ("command", Shape::Value),
    ("args", Shape::Value),
    ("health", Shape::Table(&HEALTH)),
    ("http", Shape::Table(&HTTP)),
]);

static STRATEGY: TableSchema = table(&[
    ("type", Shape::Value),
    ("partition", Shape::Value),
    ("max_surge", Shape::Value),
    ("max_unavailable", Shape::Value),
]);
static DEPLOY_ENV: TableSchema = table(&[
    ("replicas", Shape::Value),
    ("namespace", Shape::Value),
    ("mesh", Shape::Value),
    ("mcp_sidecar", Shape::Value),
    ("expose", Shape::Value),
    ("termination_grace_period_seconds", Shape::Value),
    ("strategy", Shape::Table(&STRATEGY)),
    ("image_pull_secrets", Shape::Value),
]);
static DEPLOY: TableSchema = with_overlay(
    &[
        ("replicas", Shape::Value),
        ("namespace", Shape::Value),
        ("mesh", Shape::Value),
        ("mcp_sidecar", Shape::Value),
        ("expose", Shape::Value),
        ("kind", Shape::Value),
        ("pod_management_policy", Shape::Value),
        ("termination_grace_period_seconds", Shape::Value),
        ("strategy", Shape::Table(&STRATEGY)),
        ("image_pull_secrets", Shape::Value),
    ],
    &DEPLOY_ENV,
);

static RESOURCE_LIST: TableSchema = table(&[
    ("cpu", Shape::Value),
    ("memory", Shape::Value),
    ("ephemeral_storage", Shape::Value),
]);
static RESOURCES: TableSchema = table(&[
    ("cpu", Shape::Value),
    ("memory", Shape::Value),
    ("requests", Shape::Table(&RESOURCE_LIST)),
    ("limits", Shape::Table(&RESOURCE_LIST)),
]);

static AUTOSCALE_ENV: TableSchema = table(&[
    ("enabled", Shape::Value),
    ("min_replicas", Shape::Value),
    ("max_replicas", Shape::Value),
]);
static AUTOSCALE: TableSchema = with_overlay(
    &[
        ("enabled", Shape::Value),
        ("min_replicas", Shape::Value),
        ("max_replicas", Shape::Value),
    ],
    &AUTOSCALE_ENV,
);

/// `[database]` fields. `size`, `shared`, `version` and `name` were removed
/// in 0.15; they stay known here so the resolver can say what replaces them.
const DATABASE_FIELDS: &[(&str, Shape)] = &[
    ("engine", Shape::Value),
    ("secret", Shape::Value),
    ("key", Shape::Value),
    ("env", Shape::Value),
    ("tls", Shape::Value),
    ("ca_secret", Shape::Value),
    ("cluster", Shape::Value),
    ("labels", Shape::Free),
    ("namespace", Shape::Value),
    ("port", Shape::Value),
    ("url", Shape::Value),
    ("size", Shape::Value),
    ("shared", Shape::Value),
    ("version", Shape::Value),
    ("name", Shape::Value),
];
static DATABASE_ENV: TableSchema = table(DATABASE_FIELDS);
static DATABASE: TableSchema = with_overlay(DATABASE_FIELDS, &DATABASE_ENV);

const CACHE_FIELDS: &[(&str, Shape)] = &[
    ("engine", Shape::Value),
    ("secret", Shape::Value),
    ("key", Shape::Value),
    ("env", Shape::Value),
    ("labels", Shape::Free),
    ("namespace", Shape::Value),
    ("port", Shape::Value),
    ("url", Shape::Value),
    ("size", Shape::Value),
    ("shared", Shape::Value),
    ("name", Shape::Value),
];
static CACHE_ENV: TableSchema = table(CACHE_FIELDS);
static CACHE: TableSchema = with_overlay(CACHE_FIELDS, &CACHE_ENV);

static MIGRATIONS_ENV: TableSchema = table(&[("mode", Shape::Value)]);
static MIGRATIONS: TableSchema = with_overlay(
    &[
        ("tool", Shape::Value),
        ("dir", Shape::Value),
        ("command", Shape::Value),
        ("mode", Shape::Value),
        ("run_on", Shape::Value),
    ],
    &MIGRATIONS_ENV,
);

static EXTERNAL_STORE: TableSchema = table(&[("name", Shape::Value), ("kind", Shape::Value)]);
static SECRETS_ENV: TableSchema = table(&[("create", Shape::Value)]);
static SECRETS: TableSchema = with_overlay(
    &[
        ("provider", Shape::Value),
        ("required", Shape::Value),
        ("map", Shape::Free),
        ("external_store", Shape::Table(&EXTERNAL_STORE)),
        ("create", Shape::Value),
    ],
    &SECRETS_ENV,
);

static CONFIG: TableSchema = table(&[
    ("engine", Shape::Value),
    ("path_prefix", Shape::Value),
    ("poll_interval_seconds", Shape::Value),
    ("endpoints", Shape::Value),
    ("repo", Shape::Value),
    ("git_ref", Shape::Value),
    ("sources", Shape::Value),
]);

static METHOD_CACHE: TableSchema = table(&[("ttl_ms", Shape::Value), ("capacity", Shape::Value)]);
static CLIENT: TableSchema = table(&[
    ("coalesce", Shape::Value),
    ("proxy", Shape::Value),
    ("cache", Shape::Named(&METHOD_CACHE)),
]);

static PATH_RULE: TableSchema = table(&[
    ("pattern", Shape::Value),
    ("propagate_to_callers", Shape::Value),
    ("layer", Shape::Value),
    ("packages", Shape::Value),
    ("build_order", Shape::Value),
]);

static VOLUME: TableSchema = table(&[
    ("name", Shape::Value),
    ("type", Shape::Value),
    ("mount_path", Shape::Value),
    ("read_only", Shape::Value),
    ("size_limit", Shape::Value),
    ("medium", Shape::Value),
    ("size", Shape::Value),
    ("storage_class", Shape::Value),
    ("access_modes", Shape::Value),
]);
static PORT: TableSchema = table(&[
    ("name", Shape::Value),
    ("port", Shape::Value),
    ("protocol", Shape::Value),
    ("service", Shape::Value),
]);

/// `[storage]` (written by `tonin new --with-storage`; read by the
/// service's own code, not by the chart).
static STORAGE: TableSchema = table(&[("kind", Shape::Value)]);

/// `[env]`: `KEY = "value"` plus `[env.<env>]` tables of the same.
static ENV_OVERLAY: TableSchema = TableSchema {
    fields: &[],
    overlay: None,
    open_values: true,
    open_envs: false,
};
static ENV: TableSchema = TableSchema {
    fields: &[],
    overlay: Some(&ENV_OVERLAY),
    open_values: true,
    open_envs: false,
};

/// A `[depends_on]` entry in table form; other plain keys are per-env
/// namespace overrides (`prd = "ns"`).
static DEPENDENCY: TableSchema = TableSchema {
    fields: &[
        ("namespace", Shape::Value),
        ("port", Shape::Value),
        ("ports", Shape::Value),
        ("envs", Shape::Value),
    ],
    overlay: None,
    open_values: true,
    open_envs: true,
};

/// A caller table in `[callers]` (or a `[callers.<env>]` overlay).
static CALLER: TableSchema = table(&[
    ("namespace", Shape::Value),
    ("labels", Shape::Free),
    ("service", Shape::Value),
    ("ports", Shape::Value),
]);

static FQDN: TableSchema = table(&[
    ("name", Shape::Value),
    ("pattern", Shape::Value),
    ("ports", Shape::Value),
]);
static CIDR: TableSchema = table(&[
    ("cidr", Shape::Value),
    ("except", Shape::Value),
    ("ports", Shape::Value),
]);
const EGRESS_FIELDS: &[(&str, Shape)] = &[
    ("fqdns", Shape::Array(&FQDN)),
    ("cidrs", Shape::Array(&CIDR)),
    ("kube_apiserver", Shape::Value),
];
static EGRESS_ENV: TableSchema = table(EGRESS_FIELDS);
static EGRESS: TableSchema = with_overlay(EGRESS_FIELDS, &EGRESS_ENV);

/// Tolerations and spread constraints are checked by the placement resolver
/// (they accept snake_case and camelCase Kubernetes keys).
const PLACEMENT_FIELDS: &[(&str, Shape)] = &[
    ("node_selector", Shape::Free),
    ("tolerations", Shape::Value),
    ("topology_spread", Shape::Value),
];
static PLACEMENT_ENV: TableSchema = table(PLACEMENT_FIELDS);
static PLACEMENT: TableSchema = with_overlay(PLACEMENT_FIELDS, &PLACEMENT_ENV);

const PDB_FIELDS: &[(&str, Shape)] = &[
    ("enabled", Shape::Value),
    ("min_available", Shape::Value),
    ("max_unavailable", Shape::Value),
];
static PDB_ENV: TableSchema = table(PDB_FIELDS);
static PDB: TableSchema = with_overlay(PDB_FIELDS, &PDB_ENV);

static SA_TOKEN: TableSchema = table(&[
    ("audience", Shape::Value),
    ("path", Shape::Value),
    ("expiration_seconds", Shape::Value),
]);
static SERVICE_ACCOUNT: TableSchema = table(&[
    ("automount_token", Shape::Value),
    ("token", Shape::Table(&SA_TOKEN)),
    ("tokens", Shape::Array(&SA_TOKEN)),
]);

static CRON_RESOURCES: TableSchema = table(&[("cpu", Shape::Value), ("memory", Shape::Value)]);
static CRONJOB: TableSchema = table(&[
    ("name", Shape::Value),
    ("schedule", Shape::Value),
    ("time_zone", Shape::Value),
    ("command", Shape::Value),
    ("args", Shape::Value),
    ("concurrency_policy", Shape::Value),
    ("successful_jobs_history_limit", Shape::Value),
    ("failed_jobs_history_limit", Shape::Value),
    ("starting_deadline_seconds", Shape::Value),
    ("active_deadline_seconds", Shape::Value),
    ("backoff_limit", Shape::Value),
    ("suspend", Shape::Value),
    ("resources", Shape::Table(&CRON_RESOURCES)),
    ("env", Shape::Free),
    ("envs", Shape::Value),
]);

/// The whole `tonin.toml`.
pub(crate) static TONIN_TOML: TableSchema = table(&[
    ("schema", Shape::Value),
    ("service", Shape::Table(&SERVICE)),
    ("deploy", Shape::Table(&DEPLOY)),
    ("resources", Shape::Table(&RESOURCES)),
    ("autoscale", Shape::Table(&AUTOSCALE)),
    ("depends_on", Shape::NamedOr(&DEPENDENCY)),
    ("env", Shape::Table(&ENV)),
    ("callers", Shape::Callers),
    ("egress", Shape::Table(&EGRESS)),
    ("database", Shape::Table(&DATABASE)),
    ("databases", Shape::Named(&DATABASE)),
    ("cache", Shape::Table(&CACHE)),
    ("caches", Shape::Named(&CACHE)),
    ("secrets", Shape::Table(&SECRETS)),
    ("migrations", Shape::Table(&MIGRATIONS)),
    ("config", Shape::Table(&CONFIG)),
    ("client", Shape::Table(&CLIENT)),
    ("image", Shape::Table(&IMAGE)),
    ("security", Shape::Table(&SECURITY)),
    ("path_rules", Shape::Array(&PATH_RULE)),
    ("volumes", Shape::Array(&VOLUME)),
    ("ports", Shape::Array(&PORT)),
    ("placement", Shape::Table(&PLACEMENT)),
    ("pdb", Shape::Table(&PDB)),
    ("service_account", Shape::Table(&SERVICE_ACCOUNT)),
    ("cronjobs", Shape::Array(&CRONJOB)),
    ("storage", Shape::Table(&STORAGE)),
]);

#[cfg(test)]
mod tests {
    use super::*;

    static INNER: TableSchema = TableSchema {
        fields: &[("a", Shape::Value)],
        overlay: None,
        open_values: false,
        open_envs: false,
    };
    static OVERLAY: TableSchema = TableSchema {
        fields: &[("n", Shape::Value)],
        overlay: None,
        open_values: false,
        open_envs: false,
    };
    static SECTION: TableSchema = TableSchema {
        fields: &[("n", Shape::Value), ("inner", Shape::Table(&INNER))],
        overlay: Some(&OVERLAY),
        open_values: false,
        open_envs: false,
    };
    static ROOT: TableSchema = TableSchema {
        fields: &[
            ("section", Shape::Table(&SECTION)),
            ("list", Shape::Array(&INNER)),
            ("named", Shape::Named(&INNER)),
            ("free", Shape::Free),
        ],
        overlay: None,
        open_values: false,
        open_envs: false,
    };

    fn check(src: &str) -> Result<(), SchemaError> {
        validate(src, &ROOT).map(|_| ())
    }

    #[test]
    fn accepts_known_keys_overlays_and_free_tables() {
        check(
            "[section]\nn = 1\n[section.inner]\na = 1\n[section.prd]\nn = 2\n\
             [[list]]\na = 1\n[named.x]\na = 2\n[free]\nanything = { goes = 1 }\n",
        )
        .unwrap();
    }

    #[test]
    fn reports_line_key_table_and_allowed_keys() {
        let err = check("[section]\nn = 1\nm = 2\n").unwrap_err();
        assert_eq!((err.line, err.column), (3, 1));
        assert_eq!(err.key, "section.m");
        assert_eq!(err.table, "[section]");
        assert_eq!(err.allowed, ["n", "inner", "[section.<env>] overlays"]);
        let msg = err.to_string();
        assert!(
            msg.starts_with("line 3, column 1: unknown key `section.m` in [section]"),
            "{msg}"
        );
    }

    #[test]
    fn checks_overlays_arrays_named_tables_and_the_top_level() {
        let err = check("[section.prd]\nm = 1\n").unwrap_err();
        assert_eq!(
            (err.key.as_str(), err.table.as_str()),
            ("section.prd.m", "[section.prd]")
        );
        let err = check("[[list]]\na = 1\n[[list]]\nb = 1\n").unwrap_err();
        assert_eq!((err.line, err.table.as_str()), (4, "[[list]]"));
        let err = check("[named.x]\nb = 1\n").unwrap_err();
        assert_eq!(err.key, "named.x.b");
        let err = check("[[list]]\na = 1\n[[list]]\nb = 1\n").unwrap_err();
        assert_eq!(err.key, "list[1].b");
        let err = check("typo = 1\n").unwrap_err();
        assert_eq!(err.table, "the top level");
        let err = check("[section.inner]\nz = 1\n").unwrap_err();
        assert_eq!(err.key, "section.inner.z");
        // An overlay key must look like an environment name.
        let err = check("[section.Prd]\nn = 1\n").unwrap_err();
        assert!(err.reason.contains("not an environment name"), "{err}");
        // Inline tables are checked too.
        let err = check("section = { n = 1, q = 2 }\n").unwrap_err();
        assert_eq!(err.key, "section.q");
    }
}

#[cfg(test)]
mod tonin_toml_tests {
    use super::*;

    #[test]
    fn collects_overlay_envs_and_checks_every_section() {
        let src = r#"
[service]
name = "x"
version = "1"
[deploy]
replicas = 1
namespace = "a"
[deploy.prd]
replicas = 2
[env]
LOG = "json"
[env.dev]
LOG = "pretty"
[depends_on]
identity = { namespace = "a", prod = "b" }
[callers]
api = "a"
cloudflared = { namespace = "edge", labels = { app = "cf" }, ports = [4318] }
[callers.stg]
api = "b"
[egress]
fqdns = [{ name = "api.stripe.com", ports = [443] }]
[[volumes]]
name = "tmp"
type = "empty-dir"
mount_path = "/tmp"
[resources]
cpu = "1"
memory = "1Gi"
"#;
        let envs = overlay_envs(src).unwrap();
        let names: Vec<_> = envs.iter().map(|e| e.env.as_str()).collect();
        assert_eq!(names, ["prd", "dev", "prod", "stg"]);
        assert_eq!(envs[0].location, "[deploy.prd]");
        assert_eq!(envs[2].location, "depends_on.identity.prod");

        for (bad, key) in [
            (
                "[egress]\nfqdns = [{ host = \"x\" }]\n",
                "egress.fqdns[0].host",
            ),
            (
                "[callers]\ncf = { namespace = \"e\", label = { a = \"b\" } }\n",
                "callers.cf.label",
            ),
            (
                "[callers.prd]\ncf = { namespace = \"e\", prts = [1] }\n",
                "callers.prd.cf.prts",
            ),
            (
                "[[volumes]]\nname = \"x\"\nmountpath = \"/x\"\n",
                "volumes[0].mountpath",
            ),
            (
                "[[cronjobs]]\nname = \"x\"\nschedul = \"* * * * *\"\n",
                "cronjobs[0].schedul",
            ),
            ("[database.dev]\nurll = \"x\"\n", "database.dev.urll"),
            ("[pdb.prd]\nmin_avail = 1\n", "pdb.prd.min_avail"),
            (
                "[service.health.startup]\nperiod = 1\n",
                "service.health.startup.period",
            ),
            ("[resources.limits]\ngpu = \"1\"\n", "resources.limits.gpu"),
            (
                "[depends_on]\nx = { namespace = \"a\", Port = 1 }\n",
                "depends_on.x.Port",
            ),
        ] {
            let err = overlay_envs(bad).unwrap_err();
            assert_eq!(err.key, key, "{bad}: {err}");
            assert!(
                !err.allowed.is_empty() || err.reason.contains("environment"),
                "{err}"
            );
        }
    }
}

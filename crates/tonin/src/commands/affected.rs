use anyhow::{Context, Result};
use clap::{Args, ValueEnum};
use serde::Serialize;
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::process::Command;
use tonin_plugin::plan::Plan;

#[derive(Args)]
pub struct AffectedArgs {
    /// Git ref to diff against (commit SHA, tag, or branch).
    #[arg(long)]
    pub base: String,

    /// Git ref for HEAD (default: current HEAD).
    #[arg(long, default_value = "HEAD")]
    pub head: String,

    /// Root directory containing tonin.toml files to scan.
    /// Defaults to the current directory.
    #[arg(long)]
    pub workspace: Option<PathBuf>,

    /// Environment overlay to use when loading tonin.toml files (required).
    #[arg(long)]
    pub env: String,

    /// Output format.
    #[arg(long, value_enum, default_value = "text")]
    pub format: OutputFormat,

    /// Granularity of the affected output.
    #[arg(long, value_enum, default_value = "both")]
    pub granularity: Granularity,

    /// Print detailed information (such as the list of changed files).
    #[arg(long, short = 'v')]
    pub verbose: bool,
}

#[derive(ValueEnum, Clone, Copy, PartialEq, Eq)]
pub enum OutputFormat {
    Text,
    Json,
}

#[derive(ValueEnum, Clone, Copy, PartialEq, Eq, Default)]
pub enum Granularity {
    #[default]
    Both,
    Service,
    Package,
}

#[derive(Serialize)]
pub struct AffectedResult {
    pub base: String,
    pub head: String,
    pub changed_paths: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub affected_services: Option<Vec<AffectedService>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub affected_packages: Option<Vec<AffectedPackage>>,
}

#[derive(Serialize, Clone, Debug, PartialEq, Eq)]
pub struct AffectedService {
    pub service: String,
    pub wave: usize,
    pub reason: String,
}

#[derive(Serialize, Clone, Debug, PartialEq, Eq)]
pub struct AffectedPackage {
    pub service: String,
    pub wave: usize,
    pub package: String,
    pub layer: String,
    pub build_order: usize,
    pub reason: String,
}

pub fn run(args: AffectedArgs) -> Result<()> {
    let root = super::workspace_or_cwd(args.workspace.clone())?;

    // 1. Get changed files from git
    let changed_paths = git_diff(&args.base, &args.head, &root)?;

    // 2. Load all service plans
    let plans = Plan::load_workspace_with_env(&root, &args.env)
        .with_context(|| format!("scanning tonin.toml files under {}", root.display()))?;

    // 3. Compute affected items
    let result = compute_affected(
        changed_paths,
        &plans,
        args.granularity,
        args.base,
        args.head,
    );

    match args.format {
        OutputFormat::Json => {
            println!("{}", serde_json::to_string_pretty(&result)?);
        }
        OutputFormat::Text => {
            print_text_table(&result, args.granularity, args.verbose);
        }
    }

    Ok(())
}

pub fn compute_affected(
    changed_paths: Vec<String>,
    plans: &[Plan],
    granularity: Granularity,
    base: String,
    head: String,
) -> AffectedResult {
    let wave_map = compute_waves(plans);

    let mut queue = VecDeque::new();
    let mut affected_services = HashMap::new(); // service_name -> reason
    let mut affected_packages = HashMap::new(); // (service_name, package_name) -> (reason, layer, build_order)

    // Direct matches
    for plan in plans {
        for rule in &plan.path_rules {
            let mut matched = false;
            for path in &changed_paths {
                if glob_match(&rule.pattern, path) {
                    matched = true;
                    break;
                }
            }
            if matched {
                let rule_layer = rule.layer.as_deref().unwrap_or("server");
                if rule_layer != "client" && rule_layer != "proto" && rule_layer != "test" {
                    affected_services
                        .insert(plan.name.clone(), format!("direct match: {}", rule.pattern));
                }

                // Add all packages specified in the rule
                for pkg in &rule.packages {
                    let (layer, bo) = get_package_info(plan, pkg);
                    let layer = layer
                        .or_else(|| rule.layer.clone())
                        .unwrap_or_else(|| "unknown".to_string());
                    let bo = bo.or(rule.build_order).unwrap_or(0);
                    affected_packages.insert(
                        (plan.name.clone(), pkg.clone()),
                        (format!("direct match: {}", rule.pattern), layer, bo),
                    );
                }

                // If this rule propagates, enqueue it for propagation
                if rule.propagate_to_callers {
                    queue.push_back((plan.name.clone(), rule.layer.clone().unwrap_or_default()));
                }
            }
        }
    }

    // Transitive propagation
    let mut visited_propagation = HashSet::new();
    while let Some((svc_name, dep_layer)) = queue.pop_front() {
        if visited_propagation.contains(&(svc_name.clone(), dep_layer.clone())) {
            continue;
        }
        visited_propagation.insert((svc_name.clone(), dep_layer.clone()));

        // Find the plan for this service to get its callers. Only tonin
        // services propagate; namespace/label callers (cloudflared, …) are
        // not built from this workspace.
        if let Some(plan) = plans.iter().find(|p| p.name == svc_name) {
            for caller_ref in plan.callers.iter().filter_map(|c| c.service()) {
                // Find the plan for the caller
                if let Some(caller_plan) = plans.iter().find(|p| p.name == caller_ref.name) {
                    let reason = format!("propagated from {} ({})", svc_name, dep_layer);
                    affected_services.insert(caller_plan.name.clone(), reason.clone());

                    // Mark all non-proto, non-test packages in the caller affected
                    for caller_rule in &caller_plan.path_rules {
                        let layer_str = caller_rule.layer.as_deref().unwrap_or("");
                        if layer_str != "proto" && layer_str != "test" {
                            for pkg in &caller_rule.packages {
                                let (layer, bo) = get_package_info(caller_plan, pkg);
                                let layer = layer
                                    .or_else(|| caller_rule.layer.clone())
                                    .unwrap_or_else(|| "unknown".to_string());
                                let bo = bo.or(caller_rule.build_order).unwrap_or(0);
                                affected_packages.insert(
                                    (caller_plan.name.clone(), pkg.clone()),
                                    (reason.clone(), layer, bo),
                                );
                            }

                            // If this caller rule propagates to its callers, enqueue it
                            if caller_rule.propagate_to_callers {
                                queue.push_back((
                                    caller_plan.name.clone(),
                                    caller_rule.layer.clone().unwrap_or_default(),
                                ));
                            }
                        }
                    }
                }
            }
        }
    }

    // Sort outputs
    let mut final_services = Vec::new();
    for (svc_name, reason) in affected_services {
        let wave = *wave_map.get(&svc_name).unwrap_or(&0);
        final_services.push(AffectedService {
            service: svc_name,
            wave,
            reason,
        });
    }
    final_services.sort_by(|a, b| a.wave.cmp(&b.wave).then(a.service.cmp(&b.service)));

    let mut final_packages = Vec::new();
    for ((svc_name, pkg_name), (reason, layer, bo)) in affected_packages {
        let wave = *wave_map.get(&svc_name).unwrap_or(&0);
        final_packages.push(AffectedPackage {
            service: svc_name,
            wave,
            package: pkg_name,
            layer,
            build_order: bo,
            reason,
        });
    }
    final_packages.sort_by(|a, b| {
        a.wave
            .cmp(&b.wave)
            .then(a.service.cmp(&b.service))
            .then(a.build_order.cmp(&b.build_order))
            .then(a.package.cmp(&b.package))
    });

    AffectedResult {
        base,
        head,
        changed_paths,
        affected_services: if granularity == Granularity::Service
            || granularity == Granularity::Both
        {
            Some(final_services)
        } else {
            None
        },
        affected_packages: if granularity == Granularity::Package
            || granularity == Granularity::Both
        {
            Some(final_packages)
        } else {
            None
        },
    }
}

pub(crate) fn git_diff(base: &str, head: &str, root: &std::path::Path) -> Result<Vec<String>> {
    git_diff_with(Command::new("git"), base, head, root)
}

/// [`git_diff`] with a caller-supplied base `git` command, so tests can pass
/// one isolated from the enclosing repository's environment.
fn git_diff_with(
    mut git: Command,
    base: &str,
    head: &str,
    root: &std::path::Path,
) -> Result<Vec<String>> {
    let output = git
        .args(["diff", "--name-only", &format!("{}..{}", base, head)])
        .current_dir(root)
        .output()
        .context("running git diff")?;

    if !output.status.success() {
        anyhow::bail!(
            "git diff failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let paths = String::from_utf8(output.stdout)?
        .lines()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();

    Ok(paths)
}

fn get_package_info(plan: &Plan, package_name: &str) -> (Option<String>, Option<usize>) {
    let mut best_rule = None;
    let mut min_pkg_count = usize::MAX;
    for rule in &plan.path_rules {
        if rule.packages.contains(&package_name.to_string()) && rule.packages.len() < min_pkg_count
        {
            min_pkg_count = rule.packages.len();
            best_rule = Some(rule);
        }
    }
    if let Some(rule) = best_rule {
        (rule.layer.clone(), rule.build_order)
    } else {
        (None, None)
    }
}

fn compute_waves(plans: &[Plan]) -> HashMap<String, usize> {
    let mut waves = HashMap::new();
    let name_to_deps: HashMap<&str, Vec<&str>> = plans
        .iter()
        .map(|p| {
            (
                p.name.as_str(),
                p.depends_on.iter().map(|d| d.name.as_str()).collect(),
            )
        })
        .collect();

    let mut remaining: Vec<&str> = plans.iter().map(|p| p.name.as_str()).collect();
    let mut wave = 0;
    while !remaining.is_empty() {
        let leaves: Vec<&str> = remaining
            .iter()
            .copied()
            .filter(|&n| {
                name_to_deps
                    .get(n)
                    .map(|d| d.iter().all(|dep| waves.contains_key(*dep)))
                    .unwrap_or(true)
            })
            .collect();
        if leaves.is_empty() {
            // Cycle fallback
            for r in remaining {
                waves.insert(r.to_string(), wave);
            }
            break;
        }
        for leaf in &leaves {
            waves.insert(leaf.to_string(), wave);
        }
        remaining.retain(|n| !waves.contains_key(*n));
        wave += 1;
    }
    waves
}

fn glob_match(pattern: &str, path: &str) -> bool {
    let mut regex_str = String::new();
    let mut chars = pattern.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '*' => {
                if chars.peek() == Some(&'*') {
                    chars.next();
                    regex_str.push_str(".*");
                } else {
                    regex_str.push_str("[^/]*");
                }
            }
            '?' => {
                regex_str.push_str("[^/]");
            }
            '.' | '+' | '(' | ')' | '[' | ']' | '{' | '}' | '^' | '$' | '|' | '\\' => {
                regex_str.push('\\');
                regex_str.push(c);
            }
            _ => {
                regex_str.push(c);
            }
        }
    }
    if let Ok(re) = regex::Regex::new(&format!("^{}$", regex_str)) {
        re.is_match(path)
    } else {
        false
    }
}

fn print_text_table(result: &AffectedResult, granularity: Granularity, verbose: bool) {
    if verbose {
        println!("Changed files:");
        for path in &result.changed_paths {
            println!("  - {path}");
        }
        println!();
    }

    println!(
        "Affected items ({} changed paths vs {}):",
        result.changed_paths.len(),
        result.base
    );
    println!();

    match granularity {
        Granularity::Service => {
            let svcs = result.affected_services.as_ref().unwrap();
            print_services_table(svcs);
        }
        Granularity::Package => {
            let pkgs = result.affected_packages.as_ref().unwrap();
            print_packages_table(pkgs);
        }
        Granularity::Both => {
            let svcs = result.affected_services.as_ref().unwrap();
            println!("Services:");
            print_services_table(svcs);
            println!();
            let pkgs = result.affected_packages.as_ref().unwrap();
            println!("Packages:");
            print_packages_table(pkgs);
        }
    }
}

fn print_services_table(svcs: &[AffectedService]) {
    if svcs.is_empty() {
        println!("  (none — no deployable service affected)");
        return;
    }
    println!("  {:<25} {:<6} REASON", "SERVICE", "WAVE");
    println!("  {}", "-".repeat(70));
    for svc in svcs {
        println!("  {:<25} {:<6} {}", svc.service, svc.wave, svc.reason);
    }
}

fn print_packages_table(pkgs: &[AffectedPackage]) {
    if pkgs.is_empty() {
        println!("  (none — no package affected)");
        return;
    }
    println!(
        "  {:<20} {:<6} {:<20} {:<10} {:<6} REASON",
        "SERVICE", "WAVE", "PACKAGE", "LAYER", "ORDER"
    );
    println!("  {}", "-".repeat(95));
    for pkg in pkgs {
        println!(
            "  {:<20} {:<6} {:<20} {:<10} {:<6} {}",
            pkg.service, pkg.wave, pkg.package, pkg.layer, pkg.build_order, pkg.reason
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tonin_plugin::plan::{
        ClientSpec, DEFAULT_DEPENDENCY_PORT, DependencyRef, McpMode, Mesh, PathRule, ServiceKind,
        ServiceRef,
    };
    use tonin_plugin::{Caller, CallerSource};

    fn make_mock_plan(name: &str, depends_on: &[&str], path_rules: Vec<PathRule>) -> Plan {
        Plan {
            name: name.to_string(),
            version: "0.1.0".to_string(),
            language: "rust".to_string(),
            kind: ServiceKind::Backend,
            web_mode: None,
            port: 50051,
            http_port: None,
            health: None,
            namespace: "demo".to_string(),
            mesh: Mesh::Cilium,
            replicas: 1,
            max_replicas: 1,
            mcp_mode: McpMode::InProcess,
            needs_outbound_proxy: false,
            expose: None,
            cpu: "100m".to_string(),
            memory: "128Mi".to_string(),
            image: "mock".to_string(),
            image_registry: None,
            security: None,
            depends_on: depends_on
                .iter()
                .map(|d| DependencyRef {
                    name: d.to_string(),
                    namespace: "demo".to_string(),
                    ports: vec![DEFAULT_DEPENDENCY_PORT],
                })
                .collect(),
            callers: Vec::new(),
            egress: Default::default(),
            dir: PathBuf::from("."),
            database: None,
            named_databases: Vec::new(),
            cache: None,
            named_caches: Vec::new(),
            secrets: None,
            migrations: None,
            config: None,
            emitted_env: Default::default(),
            env_vars: Vec::new(),
            selected_env: "dev".to_string(),
            client: ClientSpec::default(),
            workload: Default::default(),
            path_rules,
            pod: Default::default(),
        }
    }

    fn mock_workspace() -> Vec<Plan> {
        let auth_rules = vec![
            PathRule {
                pattern: "auth-service/auth-server/**".to_string(),
                propagate_to_callers: false,
                layer: Some("server".to_string()),
                packages: vec!["auth-server".to_string()],
                build_order: Some(2),
            },
            PathRule {
                pattern: "auth-service/auth-proto/**".to_string(),
                propagate_to_callers: true,
                layer: Some("proto".to_string()),
                packages: vec![
                    "auth-proto".to_string(),
                    "auth-client".to_string(),
                    "auth-server".to_string(),
                ],
                build_order: Some(0),
            },
            PathRule {
                pattern: "auth-service/auth-client/**".to_string(),
                propagate_to_callers: true,
                layer: Some("client".to_string()),
                packages: vec!["auth-client".to_string()],
                build_order: Some(1),
            },
        ];

        let logging_rules = vec![
            PathRule {
                pattern: "logging-service/logging-server/**".to_string(),
                propagate_to_callers: false,
                layer: Some("server".to_string()),
                packages: vec!["logging-server".to_string()],
                build_order: Some(2),
            },
            PathRule {
                pattern: "logging-service/logging-proto/**".to_string(),
                propagate_to_callers: true,
                layer: Some("proto".to_string()),
                packages: vec![
                    "logging-proto".to_string(),
                    "logging-admin-client".to_string(),
                    "logging-query-client".to_string(),
                    "logging-server".to_string(),
                ],
                build_order: Some(0),
            },
            PathRule {
                pattern: "logging-service/logging-admin-client/**".to_string(),
                propagate_to_callers: true,
                layer: Some("client".to_string()),
                packages: vec!["logging-admin-client".to_string()],
                build_order: Some(1),
            },
        ];

        let gateway_rules = vec![PathRule {
            pattern: "gateway-service/**".to_string(),
            propagate_to_callers: false,
            layer: Some("consumer".to_string()),
            packages: vec![
                "gateway-rs".to_string(),
                "auth-checker".to_string(),
                "billing-engine".to_string(),
                "policy-agent".to_string(),
            ],
            build_order: Some(0),
        }];

        let mut auth = make_mock_plan("auth-service", &[], auth_rules);
        let mut logging = make_mock_plan("logging-service", &["auth-service"], logging_rules);
        let gateway = make_mock_plan(
            "gateway-service",
            &["auth-service", "logging-service"],
            gateway_rules,
        );

        // Wire callers manually
        auth.callers.push(Caller::from_service(ServiceRef {
            name: "logging-service".to_string(),
            namespace: "demo".to_string(),
        }));
        auth.callers.push(Caller::from_service(ServiceRef {
            name: "gateway-service".to_string(),
            namespace: "demo".to_string(),
        }));
        logging.callers.push(Caller::from_service(ServiceRef {
            name: "gateway-service".to_string(),
            namespace: "demo".to_string(),
        }));

        vec![auth, logging, gateway]
    }

    #[test]
    fn wave_order_3_node_dag() {
        let plans = mock_workspace();
        let waves = compute_waves(&plans);
        assert_eq!(*waves.get("auth-service").unwrap(), 0);
        assert_eq!(*waves.get("logging-service").unwrap(), 1);
        assert_eq!(*waves.get("gateway-service").unwrap(), 2);
    }

    #[test]
    fn affected_server_only_change() {
        let changed = vec!["auth-service/auth-server/src/lib.rs".to_string()];
        let plans = mock_workspace();
        let result = compute_affected(
            changed,
            &plans,
            Granularity::Package,
            "v1.3.9".into(),
            "HEAD".into(),
        );

        let pkgs = result.affected_packages.unwrap();
        assert_eq!(pkgs.len(), 1);
        assert_eq!(pkgs[0].service, "auth-service");
        assert_eq!(pkgs[0].package, "auth-server");
        assert_eq!(pkgs[0].layer, "server");
        assert_eq!(pkgs[0].wave, 0);
    }

    #[test]
    fn affected_proto_change_fans_out() {
        let changed = vec!["auth-service/auth-proto/src/lib.rs".to_string()];
        let plans = mock_workspace();
        let result = compute_affected(
            changed,
            &plans,
            Granularity::Package,
            "v1.3.9".into(),
            "HEAD".into(),
        );

        let pkgs = result.affected_packages.unwrap();
        let services: HashSet<_> = pkgs.iter().map(|p| p.service.as_str()).collect();
        assert!(services.contains("auth-service"));
        assert!(services.contains("logging-service"));
        assert!(services.contains("gateway-service"));

        // Verify package-level details in caller (logging-service)
        let log_pkgs: Vec<_> = pkgs
            .iter()
            .filter(|p| p.service == "logging-service")
            .collect();
        assert!(!log_pkgs.is_empty());
        for pkg in &log_pkgs {
            assert_ne!(pkg.package, "logging-proto");
            assert_eq!(pkg.wave, 1);
        }
    }

    #[test]
    fn label_callers_do_not_propagate() {
        // auth-service is called only by pods matched by labels (named like
        // a workspace service): a proto change must not reach gateway-service.
        let mut plans = mock_workspace();
        let auth = plans.iter_mut().find(|p| p.name == "auth-service").unwrap();
        auth.callers = vec![Caller {
            name: "gateway-service".to_string(),
            source: CallerSource::Pods {
                namespace: Some("edge".to_string()),
                labels: [("app".to_string(), "gateway".to_string())].into(),
            },
            ports: vec![443],
        }];
        let changed = vec!["auth-service/auth-proto/src/lib.rs".to_string()];
        let result = compute_affected(
            changed,
            &plans,
            Granularity::Package,
            "v1.3.9".into(),
            "HEAD".into(),
        );
        let pkgs = result.affected_packages.unwrap();
        let services: HashSet<_> = pkgs.iter().map(|p| p.service.as_str()).collect();
        assert_eq!(services, HashSet::from(["auth-service"]));
    }

    #[test]
    fn affected_client_change_excludes_server() {
        let changed = vec!["auth-service/auth-client/src/lib.rs".to_string()];
        let plans = mock_workspace();
        let result = compute_affected(
            changed,
            &plans,
            Granularity::Package,
            "v1.3.9".into(),
            "HEAD".into(),
        );

        let pkgs = result.affected_packages.unwrap();
        let services: HashSet<_> = pkgs.iter().map(|p| p.service.as_str()).collect();
        assert!(services.contains("auth-service"));
        assert!(services.contains("logging-service"));
        assert!(services.contains("gateway-service"));

        // Within auth-service, only auth-client is affected, NOT auth-server
        let auth_pkgs: Vec<_> = pkgs
            .iter()
            .filter(|p| p.service == "auth-service")
            .collect();
        assert_eq!(auth_pkgs.len(), 1);
        assert_eq!(auth_pkgs[0].package, "auth-client");
    }

    #[test]
    fn affected_e2e_change_is_empty() {
        let changed = vec!["auth-service/auth-e2e/tests/auth.rs".to_string()];
        let plans = mock_workspace();
        let result = compute_affected(
            changed,
            &plans,
            Granularity::Package,
            "v1.3.9".into(),
            "HEAD".into(),
        );
        assert!(result.affected_packages.unwrap().is_empty());
    }

    #[test]
    fn affected_gateway_only() {
        let changed = vec!["gateway-service/src/gateway/mod.rs".to_string()];
        let plans = mock_workspace();
        let result = compute_affected(
            changed,
            &plans,
            Granularity::Package,
            "v1.3.9".into(),
            "HEAD".into(),
        );

        let pkgs = result.affected_packages.unwrap();
        assert_eq!(pkgs.len(), 4); // gateway-rs, auth-checker, billing-engine, policy-agent
        for pkg in &pkgs {
            assert_eq!(pkg.service, "gateway-service");
            assert_eq!(pkg.wave, 2);
        }
    }

    #[test]
    fn glob_match_double_star() {
        assert!(glob_match(
            "auth-service/auth-server/**",
            "auth-service/auth-server/src/lib.rs"
        ));
        assert!(glob_match(
            "auth-service/auth-server/**",
            "auth-service/auth-server/deep/nested/file.rs"
        ));
        assert!(!glob_match(
            "auth-service/auth-server/**",
            "auth-service/auth-client/src/lib.rs"
        ));
    }

    /// Is `key` an env var that makes `git` look somewhere other than its
    /// working directory for the repo, index, or config? A git hook (e.g. the
    /// pre-commit hook running the test suite) exports these, and a child
    /// `git` inheriting them acts on the *enclosing* repo instead of the
    /// tempdir under test.
    fn is_git_location_env(key: &std::ffi::OsStr) -> bool {
        const VARS: &[&str] = &[
            "GIT_DIR",
            "GIT_INDEX_FILE",
            "GIT_WORK_TREE",
            "GIT_OBJECT_DIRECTORY",
            "GIT_ALTERNATE_OBJECT_DIRECTORIES",
            "GIT_COMMON_DIR",
            "GIT_PREFIX",
            "GIT_NAMESPACE",
            "GIT_CEILING_DIRECTORIES",
            "GIT_DISCOVERY_ACROSS_FILESYSTEM",
        ];
        key.to_str()
            .is_some_and(|k| k.starts_with("GIT_CONFIG") || VARS.contains(&k))
    }

    /// A `git` command for tests, isolated from the enclosing repo and the
    /// user's config: inherited repo-location / config env is dropped, global
    /// config is an empty file under `home`, system config is off, and the
    /// commit identity comes from env (no `git config` writes).
    fn isolated_git(home: &std::path::Path) -> Command {
        isolated_git_from(home, std::env::vars_os())
    }

    /// [`isolated_git`] with an explicit inherited environment (for testing
    /// the isolation itself without mutating the process env).
    fn isolated_git_from(
        home: &std::path::Path,
        inherited: impl IntoIterator<Item = (std::ffi::OsString, std::ffi::OsString)>,
    ) -> Command {
        let global_config = home.join(".gitconfig");
        if !global_config.exists() {
            std::fs::write(&global_config, "").unwrap();
        }
        let mut cmd = Command::new("git");
        cmd.env_clear();
        for (key, value) in inherited {
            if !is_git_location_env(&key) {
                cmd.env(key, value);
            }
        }
        cmd.env("HOME", home)
            .env("XDG_CONFIG_HOME", home)
            .env("GIT_CONFIG_GLOBAL", &global_config)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "Test User")
            .env("GIT_AUTHOR_EMAIL", "test@example.com")
            .env("GIT_COMMITTER_NAME", "Test User")
            .env("GIT_COMMITTER_EMAIL", "test@example.com")
            .args([
                "-c",
                "init.defaultBranch=main",
                "-c",
                "commit.gpgsign=false",
            ]);
        cmd
    }

    /// Run an isolated `git <args>` in `dir`, asserting success; returns stdout.
    fn run_git(home: &std::path::Path, dir: &std::path::Path, args: &[&str]) -> String {
        let out = isolated_git(home)
            .args(args)
            .current_dir(dir)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap()
    }

    /// Regression: tests run from a git hook inherit GIT_DIR / GIT_WORK_TREE /
    /// GIT_INDEX_FILE pointing at the real repo; `git init` + commits in the
    /// test then rewrote that repo's config (core.bare=true, [user]). The
    /// isolated command must ignore them and act on its working directory.
    #[test]
    fn isolated_git_ignores_enclosing_repo_env() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        let decoy = tmp.path().join("decoy");
        let target = tmp.path().join("target");
        for d in [&home, &decoy, &target] {
            std::fs::create_dir_all(d).unwrap();
        }
        run_git(&home, &decoy, &["init", "-q"]);
        let decoy_config = decoy.join(".git").join("config");
        let decoy_config_before = std::fs::read_to_string(&decoy_config).unwrap();

        // Env as a pre-commit hook in the decoy repo would export it.
        let hook_env = std::env::vars_os().chain([
            ("GIT_DIR".into(), decoy.join(".git").into_os_string()),
            ("GIT_WORK_TREE".into(), decoy.clone().into_os_string()),
            (
                "GIT_INDEX_FILE".into(),
                decoy.join(".git").join("index").into_os_string(),
            ),
            ("GIT_CONFIG_PARAMETERS".into(), "'core.bare'='true'".into()),
        ]);
        let hook_env: Vec<_> = hook_env.collect();
        let git = |args: &[&str]| {
            let out = isolated_git_from(&home, hook_env.clone())
                .args(args)
                .current_dir(&target)
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "git {args:?} failed: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            String::from_utf8(out.stdout).unwrap()
        };

        git(&["init", "-q"]);
        git(&["commit", "-q", "--allow-empty", "-m", "init"]);
        let toplevel = git(&["rev-parse", "--show-toplevel"]);
        assert_eq!(
            std::path::Path::new(toplevel.trim())
                .canonicalize()
                .unwrap(),
            target.canonicalize().unwrap(),
            "git acted on the repo from inherited GIT_DIR, not its working dir"
        );
        assert_eq!(git(&["config", "--local", "core.bare"]).trim(), "false");
        assert_eq!(
            std::fs::read_to_string(&decoy_config).unwrap(),
            decoy_config_before,
            "decoy repo config was modified"
        );
    }

    #[test]
    fn cli_integration_affected_service() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        let repo_path = &tmp.path().join("repo");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(repo_path).unwrap();

        // 1. Initialize git repo (identity comes from the isolated env)
        run_git(&home, repo_path, &["init", "-q"]);

        // 2. Create subdirectories and tonin.toml files
        let auth_dir = repo_path.join("auth-service");
        std::fs::create_dir_all(&auth_dir).unwrap();

        let auth_toml = r#"
schema = "v1"
[service]
name = "auth-service"
version = "0.1.0"
[deploy]
namespace = "demo"
replicas = 1
[resources]
cpu = "100m"
memory = "128Mi"
[[path_rules]]
pattern = "auth-service/src/**"
propagate_to_callers = false
layer = "server"
packages = ["auth-server"]
"#;
        std::fs::write(auth_dir.join("tonin.toml"), auth_toml).unwrap();

        // 3. Commit initial files
        run_git(&home, repo_path, &["add", "."]);
        run_git(&home, repo_path, &["commit", "-q", "-m", "initial commit"]);

        // 4. Create a modification
        let src_dir = auth_dir.join("src");
        std::fs::create_dir_all(&src_dir).unwrap();
        std::fs::write(src_dir.join("main.rs"), "fn main() {}").unwrap();

        run_git(&home, repo_path, &["add", "."]);
        run_git(&home, repo_path, &["commit", "-q", "-m", "modify src"]);

        // 5. Run the affected logic using compute_affected
        let plans = Plan::load_workspace_with_env(repo_path, "dev").unwrap();
        let result = compute_affected(
            git_diff_with(isolated_git(&home), "HEAD~1", "HEAD", repo_path).unwrap(),
            &plans,
            Granularity::Package,
            "HEAD~1".to_string(),
            "HEAD".to_string(),
        );

        let pkgs = result.affected_packages.unwrap();
        assert_eq!(pkgs.len(), 1);
        assert_eq!(pkgs[0].service, "auth-service");
        assert_eq!(pkgs[0].package, "auth-server");
        assert_eq!(pkgs[0].layer, "server");
    }

    #[test]
    fn affected_both_granularity() {
        let changed = vec!["auth-service/auth-server/src/lib.rs".to_string()];
        let plans = mock_workspace();
        let result = compute_affected(
            changed,
            &plans,
            Granularity::Both,
            "v1.3.9".into(),
            "HEAD".into(),
        );

        assert!(result.affected_services.is_some());
        assert!(result.affected_packages.is_some());

        let svcs = result.affected_services.unwrap();
        assert_eq!(svcs.len(), 1);
        assert_eq!(svcs[0].service, "auth-service");

        let pkgs = result.affected_packages.unwrap();
        assert_eq!(pkgs.len(), 1);
        assert_eq!(pkgs[0].package, "auth-server");
    }

    #[test]
    fn affected_client_change_does_not_deploy_service() {
        let changed = vec!["auth-service/auth-client/src/lib.rs".to_string()];
        let plans = mock_workspace();
        let result = compute_affected(
            changed,
            &plans,
            Granularity::Both,
            "v1.3.9".into(),
            "HEAD".into(),
        );

        let svcs = result.affected_services.unwrap();
        let svc_names: HashSet<_> = svcs.iter().map(|s| s.service.as_str()).collect();

        // auth-service itself has only a client change, so it does not redeploy!
        assert!(!svc_names.contains("auth-service"));

        // But logging-service (caller) is affected by propagation and has a server package rebuilt, so it DOES redeploy
        assert!(svc_names.contains("logging-service"));
        assert!(svc_names.contains("gateway-service"));
    }
}

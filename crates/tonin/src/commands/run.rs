use std::collections::HashMap;
use std::net::TcpStream;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::Result;
use clap::Args;
use tonin_plugin::{CargoRunBackend, Plan, RunBackend, RunDryRunBackend, RunValues, wave_groups};

#[derive(Args)]
pub struct RunArgs {
    /// Service to run (if omitted, runs the service in current directory).
    #[arg(long)]
    pub service: Option<String>,

    /// Also start all dependencies in dependency order.
    #[arg(long)]
    pub with_deps: bool,

    /// Workspace root containing tonin.toml files.
    #[arg(long)]
    pub workspace: Option<PathBuf>,

    /// Path to .env file for local environment variables (optional).
    #[arg(long)]
    pub env_file: Option<PathBuf>,

    /// Show what would run without actually running anything.
    #[arg(long)]
    pub dry_run: bool,

    /// Wait for service to be healthy (gRPC or TCP) before starting next service.
    /// Polls for up to 10 seconds per service.
    #[arg(long)]
    pub wait_healthy: bool,

    /// Start all services in background mode without watching (useful for CI/E2E testing).
    /// Without this flag, the target service is watched for changes.
    #[arg(long)]
    pub background: bool,
}

/// Check if a service is healthy by attempting to connect to its port.
fn check_service_health(plan: &Plan) -> bool {
    // Try gRPC port first (port), then HTTP port if available
    let ports = if let Some(http_port) = plan.http_port {
        vec![plan.port as u16, http_port as u16]
    } else {
        vec![plan.port as u16]
    };

    for port in ports {
        if TcpStream::connect(("127.0.0.1", port)).is_ok() {
            return true;
        }
    }
    false
}

/// Wait for a service to become healthy (up to 10 seconds).
fn wait_for_service_health(plan: &Plan, timeout_secs: u64) -> Result<()> {
    let start = Instant::now();
    let timeout = Duration::from_secs(timeout_secs);

    loop {
        if check_service_health(plan) {
            println!("✓ {} is healthy", plan.name);
            return Ok(());
        }

        if start.elapsed() >= timeout {
            println!(
                "⚠ {} failed to become healthy within {:.0}s, continuing anyway...",
                plan.name, timeout_secs
            );
            return Ok(());
        }

        std::thread::sleep(Duration::from_millis(200));
    }
}

pub fn run(args: RunArgs) -> Result<()> {
    let root = args
        .workspace
        .clone()
        .unwrap_or_else(|| std::env::current_dir().unwrap());

    let backend: Box<dyn RunBackend> = if args.dry_run {
        Box::new(RunDryRunBackend)
    } else {
        Box::new(CargoRunBackend::new())
    };

    // Load environment variables from .env file if provided
    let env_vars = load_env_file(args.env_file.as_deref())?;

    // Load all plans from the workspace
    let plans = Plan::load_workspace(&root)?;

    // Determine which service(s) to run
    let target_service = if let Some(svc) = args.service.clone() {
        svc
    } else {
        // If no service specified, use current directory service name
        std::env::current_dir()
            .ok()
            .and_then(|cwd| {
                cwd.file_name()
                    .and_then(|n| n.to_str())
                    .map(|s| s.to_string())
            })
            .unwrap_or_else(|| "unknown".to_string())
    };

    // Find the target plan
    let target_plan = plans
        .iter()
        .find(|p| p.name == target_service)
        .ok_or_else(|| anyhow::anyhow!("service '{}' not found in workspace", target_service))?;

    // Collect services to run
    let mut services_to_run = vec![target_plan.clone()];

    if args.with_deps {
        // Build dependency order using wave_groups
        let waves = wave_groups(&plans);

        // Collect all dependencies by traversing the waves
        let deps = collect_dependencies(&plans, target_plan)?;
        services_to_run.clear();

        // Add dependencies first (in topological order), then target service
        for wave in waves {
            for plan in wave {
                if deps.contains(&plan.name) {
                    services_to_run.push(plan.clone());
                }
            }
        }
        // Ensure target is last
        services_to_run.push(target_plan.clone());
    }

    println!("▶ running {} service(s)", services_to_run.len());

    // Track started services for cleanup
    let mut started_services: Vec<String> = Vec::new();

    // Start all services except the last one in the background
    let num_services = services_to_run.len();
    let result = (|| -> Result<()> {
        for (idx, plan) in services_to_run.iter().enumerate() {
            let is_last = idx == num_services - 1;
            let service_dir = plan.dir.to_string_lossy().to_string();

            let values = RunValues {
                service: plan.name.clone(),
                service_dir,
                env_vars: env_vars.clone(),
            };

            if is_last && !args.background {
                // For the last (target) service, watch for changes and block (unless in background mode)
                println!("▶ watching {} for source changes...", plan.name);
                backend.watch_service(&plan.name, &values.service_dir)?;
            } else {
                // Start service in background (all services in background mode, or all dependencies)
                backend.start_service(&values)?;
                started_services.push(plan.name.clone());

                if args.wait_healthy {
                    wait_for_service_health(plan, 10)?;
                }
            }
        }
        Ok(())
    })();

    // In background mode, a successful run means every service is meant to
    // keep running after this process exits (the caller starts them once,
    // then uses them from later steps/processes). Only tear down on error,
    // or when not backgrounding (e.g. watch_service returned after Ctrl+C).
    if result.is_err() || !args.background {
        for service_name in started_services.iter().rev() {
            let _ = backend.stop_service(service_name);
        }
    }

    result
}

/// Collect all transitive dependencies of a service.
fn collect_dependencies(plans: &[Plan], plan: &Plan) -> Result<std::collections::HashSet<String>> {
    let mut deps = std::collections::HashSet::new();

    fn collect_recursive(
        plans: &[Plan],
        plan: &Plan,
        deps: &mut std::collections::HashSet<String>,
    ) {
        for dep_ref in &plan.depends_on {
            if !deps.contains(&dep_ref.name) {
                deps.insert(dep_ref.name.clone());

                // Find the actual plan for this dependency
                if let Some(dep_plan) = plans.iter().find(|p| p.name == dep_ref.name) {
                    collect_recursive(plans, dep_plan, deps);
                }
            }
        }
    }

    collect_recursive(plans, plan, &mut deps);
    Ok(deps)
}

/// Load environment variables from a .env file.
fn load_env_file(env_file: Option<&std::path::Path>) -> Result<HashMap<String, String>> {
    let mut env_vars = HashMap::new();

    if let Some(path) = env_file {
        let content = std::fs::read_to_string(path)
            .map_err(|e| anyhow::anyhow!("failed to read .env file {}: {e}", path.display()))?;

        for line in content.lines() {
            let line = line.trim();

            // Skip empty lines and comments
            if line.is_empty() || line.starts_with('#') {
                continue;
            }

            // Parse KEY=VALUE lines
            if let Some((key, value)) = line.split_once('=') {
                let key = key.trim().to_string();
                let value = value.trim().to_string();
                // Remove quotes if present
                let value = if (value.starts_with('"') && value.ends_with('"'))
                    || (value.starts_with('\'') && value.ends_with('\''))
                {
                    value[1..value.len() - 1].to_string()
                } else {
                    value
                };
                env_vars.insert(key, value);
            }
        }
    }

    Ok(env_vars)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_load_env_file_empty() -> Result<()> {
        let env_vars = load_env_file(None)?;
        assert_eq!(env_vars.len(), 0);
        Ok(())
    }

    #[test]
    fn test_collect_dependencies_no_deps() -> Result<()> {
        // Create a mock plan with no dependencies
        let plan = Plan {
            name: "users-service".to_string(),
            version: "1.0.0".to_string(),
            language: "rust".to_string(),
            kind: tonin_plugin::ServiceKind::Backend,
            web_mode: None,
            port: 50051,
            http_port: None,
            health: None,
            namespace: "default".to_string(),
            mesh: tonin_plugin::Mesh::Cilium,
            replicas: 1,
            max_replicas: 1,
            mcp_mode: tonin_plugin::McpMode::InProcess,
            needs_outbound_proxy: false,
            expose: None,
            cpu: "100m".to_string(),
            memory: "128Mi".to_string(),
            image: "micro/users-service:1.0.0".to_string(),
            image_registry: None,
            security: None,
            depends_on: vec![],
            callers: vec![],
            dir: PathBuf::from("/services/users-service"),
            database: None,
            named_databases: vec![],
            cache: None,
            named_caches: vec![],
            secrets: None,
            migrations: None,
            config: None,
            emitted_env: Default::default(),
            selected_env: "dev".to_string(),
            client: Default::default(),
            path_rules: vec![],
        };

        let plans = vec![plan.clone()];
        let deps = collect_dependencies(&plans, &plan)?;
        assert_eq!(deps.len(), 0);
        Ok(())
    }

    #[test]
    fn test_run_args_with_service() {
        let args = RunArgs {
            service: Some("users-service".to_string()),
            with_deps: false,
            workspace: None,
            env_file: None,
            dry_run: true,
            wait_healthy: false,
            background: false,
        };
        assert_eq!(args.service, Some("users-service".to_string()));
        assert!(!args.with_deps);
        assert!(args.dry_run);
        assert!(!args.wait_healthy);
    }

    #[test]
    fn test_run_args_with_deps() {
        let args = RunArgs {
            service: Some("products-service".to_string()),
            with_deps: true,
            workspace: Some(PathBuf::from("/workspace")),
            env_file: Some(PathBuf::from(".env")),
            dry_run: false,
            wait_healthy: false,
            background: false,
        };
        assert!(args.with_deps);
        assert_eq!(args.workspace, Some(PathBuf::from("/workspace")));
        assert_eq!(args.env_file, Some(PathBuf::from(".env")));
        assert!(!args.wait_healthy);
    }

    #[test]
    fn test_run_args_wait_healthy() {
        let args = RunArgs {
            service: Some("api-service".to_string()),
            with_deps: true,
            workspace: None,
            env_file: None,
            dry_run: false,
            wait_healthy: true,
            background: false,
        };
        assert!(args.wait_healthy);
    }

    /// Helper to create a mock Plan for testing.
    fn mock_plan(name: &str, deps: Vec<&str>) -> Plan {
        use tonin_plugin::ServiceRef;

        Plan {
            name: name.to_string(),
            version: "1.0.0".to_string(),
            language: "rust".to_string(),
            kind: tonin_plugin::ServiceKind::Backend,
            web_mode: None,
            port: 50051,
            http_port: None,
            health: None,
            namespace: "default".to_string(),
            mesh: tonin_plugin::Mesh::Cilium,
            replicas: 1,
            max_replicas: 1,
            mcp_mode: tonin_plugin::McpMode::InProcess,
            needs_outbound_proxy: false,
            expose: None,
            cpu: "100m".to_string(),
            memory: "128Mi".to_string(),
            image: format!("micro/{}:1.0.0", name),
            image_registry: None,
            security: None,
            depends_on: deps
                .into_iter()
                .map(|d| ServiceRef {
                    name: d.to_string(),
                    namespace: "default".to_string(),
                })
                .collect(),
            callers: vec![],
            dir: PathBuf::from(format!("/services/{}", name)),
            database: None,
            named_databases: vec![],
            cache: None,
            named_caches: vec![],
            secrets: None,
            migrations: None,
            config: None,
            emitted_env: Default::default(),
            selected_env: "dev".to_string(),
            client: Default::default(),
            path_rules: vec![],
        }
    }

    #[test]
    fn test_collect_dependencies_transitive() -> Result<()> {
        // Create a chain: A -> B -> C
        let c = mock_plan("c-service", vec![]);
        let b = mock_plan("b-service", vec!["c-service"]);
        let a = mock_plan("a-service", vec!["b-service"]);

        let plans = vec![a.clone(), b.clone(), c.clone()];
        let deps = collect_dependencies(&plans, &a)?;

        // a depends on b, which depends on c
        assert_eq!(deps.len(), 2);
        assert!(deps.contains("b-service"));
        assert!(deps.contains("c-service"));
        Ok(())
    }

    #[test]
    fn test_collect_dependencies_multiple_direct() -> Result<()> {
        // Create: A -> B, A -> C
        let b = mock_plan("b-service", vec![]);
        let c = mock_plan("c-service", vec![]);
        let a = mock_plan("a-service", vec!["b-service", "c-service"]);

        let plans = vec![a.clone(), b.clone(), c.clone()];
        let deps = collect_dependencies(&plans, &a)?;

        assert_eq!(deps.len(), 2);
        assert!(deps.contains("b-service"));
        assert!(deps.contains("c-service"));
        Ok(())
    }

    #[test]
    fn test_collect_dependencies_missing_dep() -> Result<()> {
        // Create: A -> B (B not in plans)
        let a = mock_plan("a-service", vec!["missing-service"]);
        let plans = vec![a.clone()];

        let deps = collect_dependencies(&plans, &a)?;

        // collect_dependencies includes all dependency names, even if they're not in plans
        // The run function filters these out later
        assert_eq!(deps.len(), 1);
        assert!(deps.contains("missing-service"));
        Ok(())
    }

    #[test]
    fn test_wave_groups_linear_3_node() -> Result<()> {
        // Create linear chain: A <- B <- C
        let a = mock_plan("a-service", vec![]);
        let b = mock_plan("b-service", vec!["a-service"]);
        let c = mock_plan("c-service", vec!["b-service"]);

        let plans = vec![a, b, c];
        let waves = wave_groups(&plans);

        // Should have 3 waves for linear dependency
        assert_eq!(waves.len(), 3, "Linear 3-node chain should produce 3 waves");
        assert_eq!(
            waves[0].len(),
            1,
            "First wave should have 1 service (a-service)"
        );
        assert_eq!(
            waves[1].len(),
            1,
            "Second wave should have 1 service (b-service)"
        );
        assert_eq!(
            waves[2].len(),
            1,
            "Third wave should have 1 service (c-service)"
        );

        // Verify ordering
        assert_eq!(waves[0][0].name, "a-service");
        assert_eq!(waves[1][0].name, "b-service");
        assert_eq!(waves[2][0].name, "c-service");

        Ok(())
    }

    #[test]
    fn test_wave_groups_parallel_deps() -> Result<()> {
        // Create: B <- A, C <- A (A has two direct dependents)
        let a = mock_plan("a-service", vec![]);
        let b = mock_plan("b-service", vec!["a-service"]);
        let c = mock_plan("c-service", vec!["a-service"]);

        let plans = vec![a, b, c];
        let waves = wave_groups(&plans);

        // Should have 2 waves: A in wave 0, B and C in wave 1
        assert_eq!(waves.len(), 2, "Parallel deps should produce 2 waves");
        assert_eq!(waves[0].len(), 1, "First wave should have 1 service");
        assert_eq!(waves[1].len(), 2, "Second wave should have 2 services");

        // Wave 0 should have a-service
        assert_eq!(waves[0][0].name, "a-service");

        // Wave 1 should have b-service and c-service in any order
        let wave1_names: std::collections::HashSet<_> =
            waves[1].iter().map(|p| p.name.as_str()).collect();
        assert!(wave1_names.contains("b-service"));
        assert!(wave1_names.contains("c-service"));

        Ok(())
    }
}

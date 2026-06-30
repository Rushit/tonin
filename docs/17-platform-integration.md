# Platform Orchestration Integration

## Overview

Phase 5b bridges `tonin` (developer lifecycle CLI) with external platform orchestrators like agnitiv-platform. The `PlatformOrchestrator` trait decouples deployment logic from CLI implementation, allowing:

- **agnitiv-platform** to trigger tonin deployments without shelling out to the CLI
- **External systems** to integrate tonin as a deployment backend via a stable trait interface
- **JSON APIs** for machine-readable deployment and status responses
- **Rollback capabilities** integrated with Helm release history

The integration reuses existing tonin infrastructure:
- `wave_groups()` for dependency ordering
- `GitLockStore` for environment state
- `HelmBackend` for Kubernetes operations
- `Plan` for service definitions

## Architecture

### PlatformOrchestrator Trait

The core trait in `tonin-plugin/src/platform.rs`:

```rust
pub trait PlatformOrchestrator: Send + Sync {
    /// Deploy a wave of services (in parallel or sequentially).
    /// Returns detailed results for each deployment.
    fn deploy_wave(&self, wave: &[&Plan], env: &str) -> Result<Vec<DeployResult>>;

    /// Get the current status of all running services.
    /// Detects drift by comparing running vs desired versions.
    fn get_running_services(&self, env: &str) -> Result<Vec<ServiceStatus>>;

    /// Rollback a service to its previous release.
    fn rollback_service(&self, service: &str, env: &str) -> Result<()>;
}
```

### CliOrchestrator Implementation

The default implementation (`CliOrchestrator`) uses:
- **HelmBackend** for cluster operations (helm upgrade, rollback, list)
- **GitLockStore** to read environment state (`environments/<ENV>/lock.toml`)
- **Plan::load_workspace_with_env()** to enumerate all services

```rust
let orchestrator = CliOrchestrator::new("/path/to/workspace");
let results = orchestrator.deploy_wave(&wave, "staging")?;
```

## CLI Integration

### Commands

Three subcommands expose the platform orchestrator:

#### 1. Deploy a Service

```bash
# Deploy a single service (parsed from tonin.toml)
tonin platform deploy --env staging --service users-service

# Same, but with JSON output
tonin platform deploy --env prod --service identity --json

# Dry-run: preview without applying
tonin platform deploy --env staging --service orders --dry-run
```

**JSON Response:**
```json
{
  "service": "identity",
  "env": "staging",
  "digest": "sha256:abc123def456...",
  "status": "success",
  "timestamp": 1719792000,
  "message": null
}
```

#### 2. Check Environment Status

```bash
# List all services and their health in an environment
tonin platform status --env prod

# With JSON output
tonin platform status --env staging --json
```

**JSON Response:**
```json
[
  {
    "service": "identity",
    "env": "staging",
    "running_version": "1.2.3",
    "desired_version": "1.2.3",
    "digest": "sha256:abc123...",
    "health": "healthy",
    "timestamp": 1719792000
  },
  {
    "service": "zradar-platform",
    "env": "staging",
    "running_version": "1.1.0",
    "desired_version": "1.2.0",
    "digest": "sha256:def456...",
    "health": "degraded",
    "timestamp": 1719792000
  }
]
```

#### 3. Rollback a Service

```bash
# Rollback the previous Helm release
tonin platform rollback --service identity --env staging
```

## Integration from agnitiv-platform

### Using PlatformClient (Rust)

The `tonin-client` crate provides a Rust client:

```rust
use tonin_client::PlatformClient;

let client = PlatformClient::new();

// Check status
let services = client.get_status("staging")?;
for svc in services {
    println!("{}: {}", svc.service, svc.health);
}

// Deploy
let result = client.deploy_service("staging", "identity")?;
if result.status == "failed" {
    // Rollback on failure
    client.rollback_service("identity", "staging")?;
}
```

### Direct CLI Integration

agnitiv-platform can also invoke the commands directly via `std::process::Command`:

```rust
let output = Command::new("tonin")
    .args(["platform", "status", "--env", "prod", "--json"])
    .output()?;

let services: Vec<ServiceStatus> = serde_json::from_slice(&output.stdout)?;
```

## Type Reference

### DeployResult

Returned by `deploy_wave()`:

```rust
pub struct DeployResult {
    pub service: String,
    pub env: String,
    pub digest: String,
    pub status: DeploymentStatus,      // Success | Failed | RolledBack
    pub timestamp: u64,                // Unix seconds
    pub message: Option<String>,       // Error detail or null
}
```

### ServiceStatus

Returned by `get_running_services()`:

```rust
pub struct ServiceStatus {
    pub service: String,
    pub env: String,
    pub running_version: Option<String>,
    pub desired_version: Option<String>,
    pub digest: Option<String>,
    pub health: String,      // "healthy" | "degraded" | "unhealthy" | "not_deployed"
    pub timestamp: u64,      // Unix seconds
}
```

**Health Status Semantics:**

- **healthy** — running_version == desired_version, service is up
- **degraded** — running_version != desired_version (reconciliation in progress or stalled)
- **unhealthy** — running_version == None, desired_version == Some (deployment failed or pending)
- **not_deployed** — both None (service not in environment)

## Workflow: Coordinated Rollout

Here's a typical agnitiv-platform deployment orchestration:

```
1. agnitiv-platform calls: tonin platform status --env staging --json
   → Gets current state (e.g., identity:healthy, zradar-platform:degraded)

2. agnitiv-platform triggers: tonin platform deploy --env staging --service identity --json
   → Identity deploys successfully
   → Returns: { service: "identity", status: "success", digest: "sha256:..." }

3. agnitiv-platform polls: tonin platform status --env staging --json
   → Waits until identity health == "healthy" (via repeated calls every 5s)

4. If step 2 fails:
   agnitiv-platform calls: tonin platform rollback --service identity --env staging
   → Rolls back to previous Helm release
   → Deploys next service in wave only after status confirms healthy
```

## Integration with Phase 4b Health Checks

The `ServiceStatus.health` field integrates with tonin's phase 4b observability:

- After each successful `helm upgrade`, health is initially "degraded" (running != desired)
- As the deployment progresses, health → "healthy" (pod readiness probes pass)
- Failed deployments result in health: "unhealthy" (no running pods)

agnitiv-platform can poll `tonin platform status` to wait for deployment completion:

```rust
// Example: wait for service to become healthy
loop {
    let statuses = client.get_status("staging")?;
    if let Some(status) = statuses.iter().find(|s| s.service == "identity") {
        if status.health == "healthy" {
            break; // Deployment complete
        }
    }
    std::thread::sleep(Duration::from_secs(5));
}
```

## Error Handling

All commands use standard exit codes:

- **0** — Success (all deployments succeeded or operations completed)
- **1** — Failure (missing lock file, helm error, service not found, etc.)

Error details are included in JSON responses:

```json
{
  "service": "unknown-service",
  "env": "staging",
  "digest": "",
  "status": "failed",
  "timestamp": 1719792000,
  "message": "service unknown-service not found in tonin.toml"
}
```

## Testing

### Unit Tests

Located in `crates/tonin/src/commands/platform.rs`:

- JSON output format validation
- Dry-run mode produces no state changes
- Error cases: missing args, invalid env

### Integration Tests

Located in `crates/tonin-plugin/src/platform.rs`:

- `CliOrchestrator::deploy_wave()` with `DryRunBackend`
- `CliOrchestrator::get_running_services()` with mock lock files
- `CliOrchestrator::rollback_service()` with `DryRunBackend`

Run tests:

```bash
cargo test -p tonin-plugin platform
cargo test -p tonin platform
```

## Future Extensions

### gRPC Backend

Phase 5c could add a `GrpcOrchestrator` that calls tonin via gRPC instead of CLI:

```rust
pub struct GrpcOrchestrator {
    channel: tonic::transport::Channel,
}

impl PlatformOrchestrator for GrpcOrchestrator {
    // ... implement via protobuf service
}
```

This would eliminate CLI process spawning overhead and enable streaming deployment logs.

### Wave-Level Operations

Current implementation deploys individual services. A future extension could add:

```rust
fn deploy_wave_in_order(&self, waves: &[Vec<&Plan>], env: &str) -> Result<Vec<Vec<DeployResult>>>;
```

This would orchestrate all waves with automatic ordering and error handling.

### Health Check Customization

Future phases could support custom health check backends beyond kubectl pod status.

## See Also

- [12-kubernetes-deploy.md](12-kubernetes-deploy.md) — Helm integration details
- [04-mcp-exposure.md](04-mcp-exposure.md) — Exposing tonin operations as LLM tools

# End-to-End Testing Guide

This guide covers running tonin's end-to-end (E2E) tests on a local k3d Kubernetes cluster with zradar tracing integration.

## Overview

The E2E test suite verifies:

- **Full deployment workflow**: Deploy ecommerce services (users, orders, products) to a local cluster
- **Wave ordering**: Services deploy in correct dependency order (users → products → orders)
- **Health checks**: Services report healthy status after deployment
- **Trace integration**: Deployment events are traced with zradar baggage propagation
- **Rollback**: Failed deployments can be rolled back

## Prerequisites

1. **Docker or Podman** — Container runtime
2. **k3d** — Lightweight Kubernetes distribution
3. **kubectl** — Kubernetes CLI (auto-installed with k3d)
4. **tonin CLI** — Built from this workspace (`cargo install --path .`)
5. **(Optional) zradar** — For tracing deployment lifecycle

Install k3d:

```bash
curl -s https://raw.githubusercontent.com/k3d-io/k3d/main/install.sh | bash
```

## Quick Start

### 1. Create k3d Cluster

```bash
# Create a minimal cluster named "tonin-e2e"
k3d cluster create tonin-e2e \
  --servers 1 \
  --agents 2 \
  --port 8080:80@loadbalancer \
  --port 8443:443@loadbalancer

# Verify cluster is running
kubectl cluster-info
```

### 2. Build and Install tonin CLI

```bash
# From the tonin workspace root
cargo install --path .
tonin --version
```

### 3. Configure Workspace

Ensure your workspace has the ecommerce services configured:

```bash
# Expected structure:
# .
# ├── examples/users/tonin.toml
# ├── examples/orders/tonin.toml
# ├── examples/products/tonin.toml
# └── environments/e2e-test/lock.toml

# For now, you can run a dry-run to test configuration:
tonin platform status --env e2e-test --dry-run --json
```

### 4. Run E2E Tests

```bash
# Run all E2E tests (requires cluster running)
cargo test e2e_ -- --ignored --nocapture

# Run a specific test
cargo test e2e_deploy_wave_ordering -- --ignored --nocapture

# Show output
cargo test e2e_ -- --ignored --nocapture -- --show-output
```

## Test Details

### Test 1: Wave Ordering (`e2e_deploy_wave_ordering`)

Verifies that services deploy in the correct order:

```
Wave 1: users (no dependencies)
  └─ Wait for health: healthy
Wave 2: products (no dependencies)
  └─ Wait for health: healthy
Wave 3: orders (depends on users, products)
  └─ Wait for health: healthy

Verify: All services report health == "healthy"
```

**Expected output:**

```
=== Wave 1: Deploying users service ===
Deploy result: status=success, digest=sha256:abc123...
users: health=degraded, attempt 1/30
users: health=degraded, attempt 2/30
users: healthy after 5 attempts

=== Wave 2: Deploying products service ===
Deploy result: status=success, digest=sha256:def456...
products: health=healthy after 3 attempts

=== Wave 3: Deploying orders service ===
Deploy result: status=success, digest=sha256:ghi789...
orders: healthy after 4 attempts

=== Final Status Verification ===
Service: users, Health: healthy, Version: Some("0.1.0")
Service: products, Health: healthy, Version: Some("0.1.0")
Service: orders, Health: healthy, Version: Some("0.1.0")
SUCCESS: All services deployed and healthy!
```

### Test 2: Rollback (`e2e_rollback_on_failure`)

Simulates a deployment failure and verifies rollback:

1. Query initial status
2. Trigger deployment
3. If deployment fails, execute rollback
4. Verify status returns to pre-deployment state

### Test 3: Deployment Tracing (`e2e_deployment_tracing`)

Captures deployment lifecycle traces via zradar baggage:

1. Deploy service with baggage context
2. Query zradar for deployment traces
3. Verify trace contains deployment metadata

## Environment Variables

Configure E2E test behavior with:

| Variable | Default | Purpose |
| --- | --- | --- |
| `TONIN_WORKSPACE` | `.` | Workspace root (where tonin.toml files live) |
| `ZRADAR_ENDPOINT` | `http://localhost:4317` | zradar gRPC endpoint for trace queries |
| `K3D_CLUSTER` | `tonin-e2e` | k3d cluster name |
| `RUST_LOG` | `info` | Logging level for test output |

Example:

```bash
TONIN_WORKSPACE=/path/to/workspace \
ZRADAR_ENDPOINT=http://zradar.local:4317 \
cargo test e2e_deploy_wave_ordering -- --ignored --nocapture
```

## Common Issues

### Cluster Not Found

```
error: kubectl cluster-info failed: ...
```

**Solution:** Create cluster first:

```bash
k3d cluster create tonin-e2e
```

### tonin CLI Not Found

```
error: tonin platform deploy failed: command not found
```

**Solution:** Install tonin:

```bash
cargo install --path .
which tonin
```

### Deployment Timeout

If services don't become healthy within 60 seconds (30 attempts × 2s):

1. **Check pod status:**
   ```bash
   kubectl get pods -n e2e-test
   kubectl describe pod <pod-name> -n e2e-test
   kubectl logs <pod-name> -n e2e-test
   ```

2. **Check Helm release:**
   ```bash
   helm list -n e2e-test
   helm status tonin-users -n e2e-test
   ```

3. **Check resource availability:**
   ```bash
   kubectl top nodes
   kubectl top pods -n e2e-test
   ```

### Service Not Found in Status

Ensure lock file exists at `environments/e2e-test/lock.toml` with all services listed.

## Tracing Integration (Phase 4b + 5c)

When zradar is configured, deployment spans are captured automatically:

1. Each `deploy_service()` call creates a root span
2. Child spans track: health checks, log queries, rollback operations
3. Baggage context propagates trace_id → service logs

**Query traces:**

```bash
# List recent deployment traces
curl "http://localhost:4317/api/traces?service=tonin&operation=deploy_service"

# Inspect single trace
curl "http://localhost:4317/api/traces/<trace_id>"
```

**Example trace structure:**

```
Trace ID: 0af7651916cd43dd8448eb211c80319c
├─ Span: deploy_wave (root)
│  ├─ Span: deploy_service (users)
│  │  └─ attributes: service=users, env=e2e-test, digest=sha256:...
│  ├─ Span: wait_for_healthy (users)
│  │  └─ attributes: max_attempts=30, attempt=5
│  └─ Span: get_status (users)
│     └─ attributes: health=healthy
└─ Span: deploy_wave (orders)
   └─ (similar structure for orders)
```

## Cleanup

Remove cluster when done:

```bash
k3d cluster delete tonin-e2e
```

## Extending Tests

To add new E2E tests:

1. Add `#[test]` function to `tests/e2e_deploy.rs`
2. Use `PlatformClient` methods (deploy_service, get_status, rollback_service)
3. Mark with `#[ignore]` and document prerequisites
4. Add expected output to this guide

Example:

```rust
#[test]
#[ignore]
fn e2e_custom_scenario() {
    let client = PlatformClient::new();
    let result = client.deploy_service("e2e-test", "my-service")?;
    assert_eq!(result.status, "success");
}
```

## CI/CD Integration

For automated E2E testing in GitHub Actions:

```yaml
# .github/workflows/e2e.yml
name: E2E Tests
on: [pull_request]

jobs:
  e2e:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v3
      
      - name: Setup k3d
        uses: nk2028/setup-k3d-and-kubeconfig@v1
      
      - name: Create k3d cluster
        run: k3d cluster create tonin-e2e
      
      - name: Install tonin CLI
        run: cargo install --path .
      
      - name: Run E2E tests
        run: cargo test e2e_ -- --ignored --nocapture
```

## See Also

- [12-kubernetes-deploy.md](12-kubernetes-deploy.md) — Helm and Kubernetes integration
- [17-platform-integration.md](17-platform-integration.md) — Platform orchestrator API
- [05-telemetry.md](05-telemetry.md) — Tracing and baggage propagation

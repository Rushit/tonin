# Tonin GitHub Actions Workflows — Phase 5a CI/CD

This document explains the tonin CI/CD workflows and how to use them in your repository.

## Overview

Phase 5a introduces two primary GitHub Actions workflows for tonin-powered services:

1. **build-and-push.yml** — Build and push container images to ghcr.io (triggered on main push)
2. **test-e2e.yml** — Run end-to-end tests with multi-service setup (triggered on pull requests and main push)

Both workflows leverage tonin's orchestration capabilities to automate the build and test lifecycle.

---

## Workflow: build-and-push.yml

### Purpose

Automatically build Docker images for all services in your tonin workspace and push them to GitHub Container Registry (ghcr.io). Images are scanned for vulnerabilities using Trivy.

### Trigger Events

- **Push to main branch** — Automatic build and push after code review
- **Push to release/* branches** — Automatic build for release preparation
- **Manual dispatch** — Trigger from GitHub UI (Actions → Build and Push → Run workflow)

### What It Does

1. **Checkout code** — Includes full git history for version resolution
2. **Install dependencies** — Rust toolchain, protoc, cargo (for tonin CLI)
3. **Set up Docker Buildx** — Multi-architecture build support (amd64, arm64)
4. **Authenticate ghcr.io** — Uses `GITHUB_TOKEN` for registry access
5. **Run `tonin build --push`** — Orchestrates multi-service build:
   - Loads all service definitions from tonin.toml files in the workspace
   - Resolves version from VERSION file or git describe
   - Builds each service's Docker image
   - Captures BuildResult digest (sha256:...)
   - Pushes images to ghcr.io with auto-detected registry name
6. **Scan images with Trivy** — Detects vulnerabilities; results uploaded to GitHub Security tab
7. **Log success** — Summary of built and pushed services

### Registry Auto-Detection

The workflow automatically detects the registry from the GitHub repository context:

```
GITHUB_REPOSITORY = "rushit/tonin"
                ↓
ghcr.io/rushit/tonin/<service-name>:<version>
```

**Priority order** for registry resolution (lowest to highest):
1. Default: `ghcr.io/example`
2. GitHub Actions auto-detection: `ghcr.io/<owner>/<repo>`
3. `tonin.toml [build].registry` setting
4. `TONIN_REGISTRY` environment variable
5. `--registry` CLI flag

### Example Output

```
▶ building users-service
  pushing ghcr.io/rushit/tonin/users-service:1.0.0
▶ building orders-service
  pushing ghcr.io/rushit/tonin/orders-service:1.0.0
▶ building products-service
  pushing ghcr.io/rushit/tonin/products-service:1.0.0
✓ 3 service(s) built successfully

Trivy scan results: no critical vulnerabilities
```

### Permissions

Required repository settings:
- **Packages: write** — Push images to ghcr.io (auto-enabled with `permissions.packages`)
- **GITHUB_TOKEN** — Auto-injected by GitHub Actions; no manual secret needed

---

## Workflow: test-e2e.yml

### Purpose

Run end-to-end tests with all services running locally, using tonin's multi-service orchestration.

### Trigger Events

- **Pull requests to main** — Validate changes before merge
- **Push to main** — Verify changes after merge
- **Manual dispatch** — Trigger from GitHub UI for debugging

### What It Does

1. **Checkout code** — Includes full git history for git describe
2. **Install dependencies** — Rust toolchain, protoc, cargo, tonin CLI
3. **Build test binaries** — Compile all test targets with `cargo build --tests`
4. **Spin up dev environment** — `tonin run --with-deps --wait-healthy`:
   - Loads all service plans from tonin.toml files
   - Resolves dependency graph (e.g., orders depends on users)
   - Starts services in dependency order
   - Waits for health checks to pass before proceeding
5. **Run E2E test suite** — `cargo nextest run --profile ci e2e::`:
   - Tests assume services are accessible on localhost
   - Port mapping is auto-managed by tonin run
   - Test output is captured in artifacts
6. **Capture service logs** — `tonin logs --follow`:
   - Useful for debugging failed tests
   - Helps identify service crashes or errors
7. **Cleanup** — `tonin stop` gracefully terminates all services
8. **Summary** — Reports test results

### Dependency Graph Example

For the ecommerce example in tonin.toml:

```
users-service (no deps)
    ↓
orders-service (depends_on: users)
    ↓
products-service (no deps, but run in parallel)
```

The `--with-deps` flag ensures startup order is respected and health checks pass before moving to the next service.

### Example Output

```
Spinning up dev environment with --with-deps:
  ✓ users-service healthy (port 50051)
  ✓ products-service healthy (port 50052)
  ✓ orders-service healthy (port 50053)

Running E2E tests...
test users::create_user ... ok
test orders::create_order ... ok
test products::list_products ... ok

Test result: PASSED (3s)

Capturing logs from all services...
users-service: 2 logs captured
orders-service: 5 logs captured
products-service: 1 log captured

Cleanup: services stopped gracefully
```

### Permissions

- **Contents: read** — Read repository code (auto-enabled)

---

## Local Development: Using Workflows Locally

You can replicate the workflow behavior locally by running the same commands:

### Build and Push (Local)

```bash
# Install tonin CLI from source
cargo install --path crates/tonin

# Build all services with explicit registry override
TONIN_REGISTRY=docker.io/myusername tonin build --push

# Or use CLI flag
tonin build --registry docker.io/myusername --push

# Or use VERSION file + auto-detection (if in a git repo)
tonin build --push
```

### E2E Tests (Local)

```bash
# Start all services with dependency resolution
tonin run --with-deps --wait-healthy

# In another terminal, run tests
cargo nextest run --profile ci e2e::

# View logs from a specific service
tonin logs --follow orders-service

# Stop all services when done
tonin stop
```

---

## Configuration: tonin.toml

The `tonin.toml` file in the repository root defines:

- **[build] section** — Registry settings and image configuration
- **[services] section** — Service definitions with deployment config, resources, autoscale rules, and dependencies
- **[test] section** — E2E test command, timeout, required services, and environment variables

### Example

```toml
[build]
# registry = "ghcr.io/myorg"  # Optional: override auto-detection

[[services]]
name = "users-service"
version = "0.1.0"

[services.deploy]
replicas = 2
namespace = "shop"

[[services]]
name = "orders-service"
version = "0.1.0"

[services.depends_on]
users = "shop"

[test]
command = "cargo nextest run --profile ci e2e::"
timeout = "5m"
```

---

## Manual Workflow Dispatch

Trigger workflows manually from the GitHub UI:

1. Go to **Actions** tab in the repository
2. Select **Build and Push** or **E2E Tests**
3. Click **Run workflow**
4. Optionally select branch and enter parameters
5. Click green **Run workflow** button
6. Monitor the run in the workflow logs

### Example: Manual Build

```
Workflow: Build and Push
Branch: main
Manual trigger reason: "Release preparation for v0.2.0"
Expected output: Images pushed to ghcr.io/rushit/tonin/*:latest
```

---

## Troubleshooting

### Build Fails: "no services found to build"

**Cause:** No tonin.toml files or service definitions found in workspace.

**Solution:**
1. Ensure `tonin.toml` exists in repo root or subdirectories
2. Check that `[[services]]` tables are properly formatted
3. Verify service names match directory structure (if using Dockerfile inference)

### E2E Tests Fail: Service Connection Refused

**Cause:** Services did not start or health checks failed.

**Solution:**
1. Check `tonin logs --follow <service>` for startup errors
2. Verify service Dockerfiles are buildable locally
3. Ensure `--with-deps` respects dependency order (check `depends_on` in tonin.toml)
4. Increase `--wait-healthy` timeout if services are slow to start

### Registry Push Fails: Unauthorized

**Cause:** GitHub Container Registry authentication failed.

**Solution:**
1. Ensure `packages: write` permission is enabled in `.github/workflows/*.yml`
2. Verify `GITHUB_TOKEN` is passed to `docker/login-action`
3. Check that the repository is public or the token has appropriate scopes

### Trivy Scan Fails: No Images Found

**Cause:** Hardcoded service names in trivy step don't match your services.

**Solution:**
1. Update the trivy scan step in `build-and-push.yml` to list your actual service names
2. Or use a more flexible scan approach (e.g., query registry API for all images)

---

## Advanced: Customizing Workflows

### Override Registry for All Builds

Edit `.github/workflows/build-and-push.yml` and set environment variable:

```yaml
env:
  TONIN_REGISTRY: docker.io/myusername
```

Or update `tonin.toml`:

```toml
[build]
registry = "docker.io/myusername"
```

### Add Manual Build Trigger with Parameters

Update `.github/workflows/build-and-push.yml`:

```yaml
on:
  workflow_dispatch:
    inputs:
      registry:
        description: "Registry override (e.g., docker.io/myusername)"
        required: false
        type: string
```

Then use in build step:

```bash
tonin build --registry ${{ inputs.registry }} --push
```

### Skip Trivy Scan for Faster Builds

Remove the trivy-action step from `build-and-push.yml` or add condition:

```yaml
if: github.event_name == 'push'  # Only scan on push, not on manual dispatch
```

---

## Phase 5a: Summary

| Artifact | Purpose |
|----------|---------|
| `.github/workflows/build-and-push.yml` | CI: build and push services to ghcr.io |
| `.github/workflows/test-e2e.yml` | CI: run E2E tests with tonin run --with-deps |
| `tonin.toml` | Configuration: [build], [services], [test] sections |
| `docs/GITHUB-ACTIONS.md` | This documentation |

All workflows are production-ready and leverage tonin's Phase 4a build system (BuildBackend, BuildResult digest, registry auto-detection) and Phase 4b run system (multi-service orchestration, dependency management, health checks).

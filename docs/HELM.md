# Helm Chart Documentation

This guide covers installing and managing tonin microservices using Helm.

## Overview

The tonin Helm chart (`helm/`) automates Kubernetes deployment of tonin services with:

- **Multi-service templates** — Deploy users, orders, products services with correct dependencies
- **Autoscaling** — Horizontal pod autoscaling (HPA) based on CPU utilization
- **Health checks** — gRPC liveness and readiness probes
- **Telemetry** — OTLP endpoint configuration and zradar baggage propagation
- **Service mesh** — Cilium/Istio integration via network policies
- **Resource management** — CPU/memory requests and limits per service

## Chart Structure

```
helm/
├── Chart.yaml                 # Chart metadata (name, version, appVersion)
├── values.yaml                # Default configuration for all services
└── templates/
    ├── _helpers.tpl           # Helm template functions (labels, selectors)
    ├── deployment.yaml        # Kubernetes Deployment + Service + ServiceAccount + HPA
    └── configmap.yaml         # Shared configuration (env vars, service discovery)
```

## Quick Start

### 1. Prerequisites

- `helm` CLI (3.10+)
- `kubectl` configured to your Kubernetes cluster
- Container images available in your registry

### 2. Verify Chart Syntax

```bash
helm lint helm/
```

**Expected output:**
```
==> Linting helm/
[INFO] Chart.yaml: icon is missing
1 chart(s) linted, 0 error(s)
```

### 3. Dry-run (Preview Manifests)

```bash
# Preview all generated Kubernetes manifests
helm template tonin helm/ \
  --namespace ecommerce \
  --create-namespace

# Validate manifests against cluster API
helm template tonin helm/ | kubectl apply --dry-run=client -f -
```

### 4. Install Chart

```bash
# Install to a new namespace
helm install tonin helm/ \
  --namespace ecommerce \
  --create-namespace

# Verify installation
helm list -n ecommerce
kubectl get deployments -n ecommerce
kubectl get pods -n ecommerce
```

### 5. Verify Services are Running

```bash
# Check deployment status
kubectl rollout status deployment/tonin-users -n ecommerce
kubectl rollout status deployment/tonin-orders -n ecommerce
kubectl rollout status deployment/tonin-products -n ecommerce

# Check service connectivity (port-forward to test)
kubectl port-forward svc/tonin-users 50051:50051 -n ecommerce
# In another terminal:
grpcurl localhost:50051 list
```

## Configuration

### Common Customizations

#### 1. Override Image Registry

```bash
helm install tonin helm/ \
  --namespace ecommerce \
  --set image.registry=my-registry.azurecr.io/tonin
```

#### 2. Disable a Service

```bash
helm install tonin helm/ \
  --namespace ecommerce \
  --set services.orders.enabled=false
```

#### 3. Configure Service Replicas

```bash
helm install tonin helm/ \
  --namespace ecommerce \
  --set services.users.replicas=3 \
  --set services.orders.replicas=4
```

#### 4. Adjust Resource Limits

```bash
helm install tonin helm/ \
  --namespace ecommerce \
  --set services.users.resources.limits.memory=512Mi \
  --set services.orders.resources.limits.cpu=1000m
```

#### 5. Configure Autoscaling

```bash
helm install tonin helm/ \
  --namespace ecommerce \
  --set services.users.autoscale.minReplicas=2 \
  --set services.users.autoscale.maxReplicas=10 \
  --set services.users.autoscale.targetCPUUtilizationPercentage=60
```

#### 6. Set OTLP Endpoint (Telemetry)

```bash
helm install tonin helm/ \
  --namespace ecommerce \
  --set telemetry.otlpEndpoint=http://my-otel-collector:4317
```

### Using a Custom Values File

Create `my-values.yaml`:

```yaml
global:
  namespace: production

image:
  registry: ghcr.io/mycompany

services:
  users:
    replicas: 5
    resources:
      requests:
        cpu: 200m
        memory: 256Mi
      limits:
        cpu: 500m
        memory: 512Mi

  orders:
    replicas: 3
    autoscale:
      minReplicas: 3
      maxReplicas: 12

telemetry:
  enabled: true
  otlpEndpoint: http://otel-collector.monitoring:4317
  logLevel: debug
```

Then install:

```bash
helm install tonin helm/ \
  --namespace production \
  --create-namespace \
  -f my-values.yaml
```

## Values Reference

### Global Settings

| Key | Default | Purpose |
| --- | --- | --- |
| `global.namespace` | `default` | Kubernetes namespace for all resources |
| `global.domain` | `svc.cluster.local` | Cluster internal DNS domain |

### Image Configuration

| Key | Default | Purpose |
| --- | --- | --- |
| `image.registry` | `ghcr.io/rushit` | Container image registry |
| `image.pullPolicy` | `IfNotPresent` | Image pull policy |
| `image.tag` | Chart `appVersion` | Override image tag |

### Service Settings

| Key | Default | Purpose |
| --- | --- | --- |
| `service.type` | `ClusterIP` | Kubernetes service type |
| `service.port` | `50051` | External port |
| `service.targetPort` | `50051` | Container port |
| `service.healthCheck.enabled` | `true` | Enable gRPC health checks |
| `service.healthCheck.initialDelaySeconds` | `10` | Probe delay |
| `service.healthCheck.periodSeconds` | `10` | Probe interval |

### Resources

| Key | Default | Purpose |
| --- | --- | --- |
| `resources.requests.cpu` | `100m` | Minimum CPU per pod |
| `resources.requests.memory` | `128Mi` | Minimum memory per pod |
| `resources.limits.cpu` | `500m` | Maximum CPU per pod |
| `resources.limits.memory` | `512Mi` | Maximum memory per pod |

### Service-Specific Settings

Each service can override defaults via `services.<service-name>`:

```yaml
services:
  users:
    enabled: true                    # Deploy this service
    version: "0.1.0"                 # Service version
    replicas: 2                      # Pod replicas
    resources: {...}                 # CPU/memory overrides
    autoscale: {...}                 # HPA settings
    mesh: {enabled: true, ...}       # Service mesh config
    depends_on: []                   # Dependency list
```

### Telemetry

| Key | Default | Purpose |
| --- | --- | --- |
| `telemetry.enabled` | `true` | Enable OpenTelemetry integration |
| `telemetry.otlpEndpoint` | `http://otel-collector.observability.svc.cluster.local:4317` | OTLP collector endpoint |
| `telemetry.logLevel` | `info` | Log level (debug, info, warn, error) |
| `telemetry.zradarBaggage` | `true` | Enable zradar baggage propagation |

### MCP Sidecar

| Key | Default | Purpose |
| --- | --- | --- |
| `mcp.enabled` | `false` | Deploy MCP sidecar container |
| `mcp.image` | `ghcr.io/rushit/tonin-mcp-sidecar:0.11.0` | Sidecar image |
| `mcp.resources` | See values.yaml | Sidecar resource limits |

## Upgrade and Rollback

### Upgrade to New Version

```bash
# Update chart dependencies (if any)
helm dependency update helm/

# Perform upgrade
helm upgrade tonin helm/ \
  --namespace ecommerce

# Check rollout status
kubectl rollout status deployment/tonin-users -n ecommerce
kubectl rollout status deployment/tonin-orders -n ecommerce
```

### Rollback to Previous Release

```bash
# List release history
helm history tonin -n ecommerce

# Rollback to previous release
helm rollback tonin -n ecommerce

# Rollback to specific revision
helm rollback tonin 2 -n ecommerce
```

## Troubleshooting

### Pods Not Starting

```bash
# Check pod status
kubectl get pods -n ecommerce
kubectl describe pod tonin-users-xxx -n ecommerce

# View logs
kubectl logs tonin-users-xxx -n ecommerce
kubectl logs tonin-users-xxx -n ecommerce --previous  # After crash
```

### Image Pull Errors

```bash
# Check image availability
kubectl describe pod tonin-users-xxx -n ecommerce | grep -A 5 Events

# If using private registry, create pull secret:
kubectl create secret docker-registry regcred \
  --docker-server=ghcr.io \
  --docker-username=<username> \
  --docker-password=<token> \
  -n ecommerce

# Add to values.yaml:
imagePullSecrets:
  - name: regcred
```

### Service Discovery Not Working

```bash
# Test DNS resolution within cluster
kubectl run -it --rm debug --image=busybox --restart=Never -n ecommerce -- \
  nslookup tonin-users.ecommerce.svc.cluster.local

# Check ConfigMap was created
kubectl get configmap tonin-config -n ecommerce
kubectl get configmap tonin-config -n ecommerce -o yaml
```

### Memory/CPU Limits Exceeded

```bash
# Check current resource usage
kubectl top pods -n ecommerce
kubectl top nodes

# Increase limits in values.yaml
services:
  users:
    resources:
      limits:
        memory: 1Gi
        cpu: 1000m
```

### HPA Not Scaling

```bash
# Check HPA status
kubectl get hpa -n ecommerce
kubectl describe hpa tonin-users -n ecommerce

# Check metrics server is running
kubectl get deployment metrics-server -n kube-system

# If metrics unavailable, check pod metrics manually
kubectl top pod tonin-users-xxx -n ecommerce
```

## Uninstall

```bash
# Remove release
helm uninstall tonin -n ecommerce

# Verify all resources removed
kubectl get all -n ecommerce
```

## Advanced Scenarios

### Multi-Environment Deployments

Deploy to multiple environments with different configs:

```bash
# Development
helm install tonin-dev helm/ \
  --namespace ecommerce-dev \
  -f values-dev.yaml

# Staging
helm install tonin-stg helm/ \
  --namespace ecommerce-stg \
  -f values-staging.yaml

# Production
helm install tonin-prod helm/ \
  --namespace ecommerce-prod \
  -f values-production.yaml
```

### GitOps Integration

Use Flux or ArgoCD to manage Helm releases:

**ArgoCD Application:**
```yaml
apiVersion: argoproj.io/v1alpha1
kind: Application
metadata:
  name: tonin
  namespace: argocd
spec:
  project: default
  source:
    repoURL: https://github.com/your-org/tonin
    targetRevision: main
    path: helm
    helm:
      valueFiles:
        - values-production.yaml
      parameters:
        - name: image.registry
          value: my-registry.com
  destination:
    server: https://kubernetes.default.svc
    namespace: ecommerce
  syncPolicy:
    automated:
      prune: true
      selfHeal: true
```

### Custom Service Dependencies

If using a custom dependency graph beyond users → products → orders:

```yaml
services:
  payment:
    enabled: true
    depends_on:
      - users

  orders:
    depends_on:
      - users
      - products
      - payment

  notifications:
    depends_on:
      - orders
```

The tonin platform will respect these dependencies during deployment.

## Integration with tonin-platform

The Helm chart works with tonin's Phase 5b platform orchestration:

```bash
# Deploy via tonin CLI
tonin platform deploy --env prod --service users --json

# Get deployment status
tonin platform status --env prod --json

# Helm will be called under the hood:
helm upgrade tonin-users helm/... --namespace prod
```

## Generated service charts: values reference

The per-service charts `tonin helm generate` writes keep environment-independent settings in
`values.yaml` and everything an `[<section>.<env>]` overlay can change in `values-<env>.yaml`.
Render with both (`-f values.yaml -f values-<env>.yaml`). The keys added in 0.15:

| Key | File | From `tonin.toml` |
| --- | --- | --- |
| `workload.kind` | base | `[deploy] kind`: `Deployment` or `StatefulSet` (`templates/deployment.yaml` renders either) |
| `workload.podManagementPolicy` | base | `[deploy] pod_management_policy` (StatefulSet) |
| `workload.terminationGracePeriodSeconds` | env | `[deploy] termination_grace_period_seconds` (0 = Kubernetes default) |
| `workload.strategy` | env | `[deploy.strategy]`; `strategy` on a Deployment, `updateStrategy` (with `partition`) on a StatefulSet |
| `volumes` | base | `[[volumes]]`: `type` `empty-dir` (`emptyDir`) or `pvc` (`size`, `accessModes`, `storageClass`; StatefulSet `volumeClaimTemplates`) |
| `extraPorts`, `extraServices` | base | `[[ports]]`: named container ports; `service` puts a port on Service `<name>-<service>` |
| `service.health.livenessPath`, `readinessPath`, `startup` | base | `[service.health]` paths and `[service.health.startup]` |
| `resources` | base | `[resources]` or `[resources.requests]` / `[resources.limits]` (CPU limit optional, `ephemeral-storage`) |
| `command`, `args` | base | `[service] command` / `args`; Rust default `/usr/local/bin/<name>`, other languages the image's CMD |
| `autoscaling.minReplicas` | env | `[autoscale] min_replicas` (HPA only for Deployments) |
| `networkPolicy.callers[]`, `.dependsOn[].ports`, `.egress` | env | `[callers]`, `[depends_on]`, `[egress]` |
| `imagePullSecrets`, `nodeSelector`, `tolerations`, `topologySpreadConstraints`, `pdb`, `serviceAccount`, `cronjobs` | env / base | `[deploy] image_pull_secrets`, `[placement]`, `[pdb]`, `[service_account]`, `[[cronjobs]]` |
| `secrets.create` | env | `[secrets] create` / `[secrets.<env>] create` |

A StatefulSet chart also renders the headless Service `<name>-headless`. Field-by-field
`tonin.toml` documentation: [12-kubernetes-deploy.md](12-kubernetes-deploy.md); upgrading from
0.14: [MIGRATION-0.15.md](MIGRATION-0.15.md).

## Generated service charts: pods

Every pod a generated chart renders (Deployment, migration Job, CronJobs)
takes its ServiceAccount, `imagePullSecrets`, pod `securityContext`,
`nodeSelector` and `tolerations` from the `service.podSpecCommon` helper in
`_helpers.tpl`; the service's own pods also get `topologySpreadConstraints`.
The values come from `[deploy] image_pull_secrets`, `[placement]`, `[pdb]`,
`[service_account]` and `[[cronjobs]]` in `tonin.toml`; see
[12-kubernetes-deploy.md](12-kubernetes-deploy.md#pods-pull-secrets-serviceaccount-placement-disruption-cronjobs).
Custom templates that add a pod spec should include the same helper:

```yaml
    spec:
      {{- include "service.podSpecCommon" (dict "root" . "component" "server") | nindent 6 }}
```

## Generated service charts: databases, caches and migrations

The per-service charts `tonin helm generate` writes (see
[12-kubernetes-deploy.md](12-kubernetes-deploy.md)) do not run databases or caches. Since
tonin 0.15, `[database]` and `[cache]` in `tonin.toml` are references to a **platform contract**
that the platform layer (CNPG, a Redis manifest, or a managed cloud service) provides in the
service's namespace:

| Contract | Default name | Used for |
|---|---|---|
| Secret `<svc>-pg-app`, key `uri` | from `Cluster <svc>-pg` (CNPG) | `DATABASE_URL` |
| Secret `<svc>-pg-ca`, key `ca.crt` | from `Cluster <svc>-pg` (CNPG) | CA mount for `tls = "verify-full"` |
| Pods labelled `cnpg.io/cluster=<svc>-pg`, port 5432 | CNPG | network-policy egress |
| Secret `<svc>-cache`, key `uri` | platform Redis manifest | `REDIS_URL` |
| Pods labelled `app.kubernetes.io/name=<svc>-cache`, port 6379 | platform Redis manifest | network-policy egress |

Every name, the key, the env var, the pod labels and their namespace and port are overridable,
on the table or per environment. A service named `api` whose code reads other variable names:

```toml
[database]
engine = "postgres"
env    = "PLATFORM_DATABASE_URL"   # from api-pg-app / uri

[database.prd]
tls = "verify-full"                # mounts api-pg-ca / ca.crt at /etc/pg-ca

[database.dev]                     # dev cluster without contract Secrets
url       = "postgresql://postgres:postgres@postgres.shared-dev.svc.cluster.local:5432/api_dev"
namespace = "shared-dev"           # egress to the shared instance, not the contract pods
labels    = { "app.kubernetes.io/name" = "postgres" }

[cache]
engine = "redis"
env    = "PLATFORM_REDIS_URL"      # from api-cache / uri

[migrations]
tool    = "custom"
command = ["/usr/local/bin/api", "migrate"]
mode    = "job"                    # "job" | "init-container" | "none", per env too
```

In the chart this becomes the `datastores` list in `values-<env>.yaml` (one entry per database or
cache: `env`, `secret` {`env`, `name`, `key`}, `value`, `caMount`, `egress`) and
`migrations.mode`. With `verify-full`, the raw uri goes into a helper var (`TONIN_PG_URI`)
declared first, and `DATABASE_URL` is
`$(TONIN_PG_URI)?sslmode=verify-full&sslrootcert=/etc/pg-ca/ca.crt` (Kubernetes `$(VAR)`
expansion).

`migrations.mode: job` renders `<svc>-migrate` as a `pre-install,pre-upgrade` hook (weight `0`)
plus, under Cilium, its own hook CiliumNetworkPolicy (weight `-5`) so egress to the database is
allowed before the Job starts. Hooks run before the release's own resources exist, so the Job
depends only on the contract Secrets; `[secrets]` keys are referenced with `optional: true`. The
Job's pods do not carry the `app` label the Service selects on.

Under Cilium, the service's own policy excludes the migration Job's pods
(`app.kubernetes.io/component: migrate`), so the Job never gets the service's `[depends_on]`
or `[egress]` rules. Callers, dependencies and `[egress]` (FQDNs, CIDRs, kube-apiserver) are
described in [12-kubernetes-deploy.md](12-kubernetes-deploy.md#network-policy-callers-egress).

Charts generated by earlier versions contain `templates/db-statefulset.yaml`, `db-service.yaml`,
`cache-statefulset.yaml` and `cache-service.yaml`; `tonin helm generate` deletes them and
`tonin helm check` reports them. Full reference: [08-database.md](08-database.md),
[07-cache.md](07-cache.md).

## See Also

- [12-kubernetes-deploy.md](12-kubernetes-deploy.md) — Helm integration details
- [08-database.md](08-database.md), [07-cache.md](07-cache.md) — the database and cache contract
- [17-platform-integration.md](17-platform-integration.md) — Platform orchestrator API
- [E2E-TESTING.md](E2E-TESTING.md) — Testing deployments with the Helm chart
- [Helm Official Docs](https://helm.sh/docs/)

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

## See Also

- [12-kubernetes-deploy.md](12-kubernetes-deploy.md) — Helm integration details
- [17-platform-integration.md](17-platform-integration.md) — Platform orchestrator API
- [E2E-TESTING.md](E2E-TESTING.md) — Testing deployments with the Helm chart
- [Helm Official Docs](https://helm.sh/docs/)

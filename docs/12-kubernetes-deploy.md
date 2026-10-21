# Kubernetes deploy

One `tonin.toml` renders every manifest your service needs to ship.

## What you get

- **Every manifest a real service needs**, generated from a single TOML file:
  - `Deployment`, `Service`, `HorizontalPodAutoscaler`
  - `Ingress` when you ask for one
  - `DATABASE_URL` / `REDIS_URL` from the platform's Secrets, plus egress to the database/cache pods, when `[database]` / `[cache]` is set (tonin references databases and caches, it does not run them: see [08-database.md](08-database.md))
  - A migration initContainer or pre-install/pre-upgrade hook `Job` when `[migrations]` is set
  - Mesh-specific overlays (Cilium `CiliumNetworkPolicy`, Istio `PeerAuthentication`, Linkerd annotations) when `[deploy].mesh` is set
- **`tonin helm generate`** writes them as a complete Helm chart inside the `./chart/` directory. **`tonin helm upgrade`** ships them. No hand-edited YAML, ever — the next `generate` would overwrite it.
- **Multi-env overlays.** The same `tonin.toml` describes every environment (`dev`, `prd`, …) — `--env` picks the right block. `--env` is required: tonin has no default environment.
- **Workspace mode.** One command walks every `tonin.toml` under a directory and renders the cross-service dependency graph.

## Complete `tonin.toml` example

Everything below comes from one file. The CLI parses it once and decides which templates to render.

```toml
[service]
name = "greeter"
version = "0.1.0"

[deploy]
replicas = 2
mesh = "cilium"
namespace = "default"
mcp_sidecar = true

[resources]
cpu = "100m"
memory = "128Mi"

[database]
engine = "postgres"

[cache]
engine = "redis"
```

That's the whole input. `tonin helm generate` turns it into a structured Helm chart with generic template files and configuration values.

By default, `tonin helm generate` uses `micro/<name>:<version>` as the container image. Override this with `[image].registry`:

```toml
[image]
registry = "ghcr.io/myorg"   # → ghcr.io/myorg/greeter:0.1.0
```

Priority (highest to lowest):
1. `TONIN_IMAGE_PREFIX` environment variable — always wins, for CI/CD per-build overrides
2. `[image].registry` in `tonin.toml` — the default for this service
3. `micro/` prefix — built-in fallback when neither is set

Declare pod and container security context natively in `tonin.toml`. When present, `tonin helm generate` emits `podSecurityContext` and `containerSecurityContext` blocks in `values.yaml` and wires them into the Deployment. When absent, no security context fields are generated — fully backward compatible.

Keys may be written in `snake_case` (auto-converted to `camelCase`) or already in `camelCase` — both work:

```toml
# Distroless / nonroot image — full security lockdown
[security.pod]
run_as_non_root = true   # → runAsNonRoot
run_as_user     = 65532  # → runAsUser  (nonroot UID used by distroless images)
run_as_group    = 65532  # → runAsGroup
fs_group        = 65532  # → fsGroup

[security.container]
allow_privilege_escalation = false   # → allowPrivilegeEscalation
read_only_root_filesystem  = true    # → readOnlyRootFilesystem

# Nested TOML tables map to nested YAML — no special syntax needed
[security.container.capabilities]
drop = ["ALL"]

[security.container.seccomp_profile]
type = "RuntimeDefault"
```

Both `[security.pod]` and `[security.container]` are optional independently — you can declare only one. Any field supported by the Kubernetes API can be used directly; new Kubernetes security context fields work without a tonin update.

## What gets rendered

The renderer (`crates/tonin/src/codegen/render.rs`) maps `tonin.toml` fields to a fixed set of output files:

| File | When |
|---|---|
| `deployment.yaml` | always: a `Deployment`, or a `StatefulSet` for `[deploy] kind = "statefulset"` |
| `service.yaml` | always: the Service, extra named Services from `[[ports]]`, and a headless Service for a StatefulSet |
| `hpa.yaml` | `[autoscale]` with a range for the environment (Deployments only) |
| `ingress.yaml` | `[deploy].expose = "ingress"` |
| `migration-job.yaml` | `[migrations]` with `mode = "job"` for the environment (plus a hook CiliumNetworkPolicy under Cilium) |
| `mesh/<engine>/*.yaml` | `[deploy].mesh = "cilium" \| "istio" \| "linkerd"` |

`[database]` and `[cache]` render no files of their own: they add env vars (from the platform
contract's Secrets), an optional CA volume and egress rules to the workloads above. Since 0.15
`generate` deletes the `db-*.yaml` / `cache-*.yaml` templates earlier versions rendered.

## Service protocol: gRPC, HTTP, or both

`[service].type` selects the primary protocol. It defaults to `backend` (gRPC), so existing services are unaffected.

| `type` | Port | Probe | Notes |
|---|---|---|---|
| `backend` (default) | gRPC `50051` | none | unchanged; MCP sidecar per `[deploy].mcp_sidecar` |
| `http` | `8080` (or `[service].port`) | `httpGet /health` | plain HTTP/REST service (e.g. axum); MCP sidecar forced off |
| `web` | `8080`/`3000` by `web_mode` | none | TypeScript SPA/BFF frontend |

`[service].port` overrides the listen port for any type. An `http` service gets a default `GET /health` liveness/readiness probe; customize it with `[service.health]`:

```toml
[service]
name = "web-api"
version = "0.1.0"
type = "http"
port = 7001

[service.health]
path = "/healthz"   # default: /health
# port = 7001       # default: the service's listen port
```

**Both gRPC and HTTP.** A gRPC `backend` can *also* expose an HTTP port (health, metrics, admin) — the two are not exclusive. Add `[service.http]`:

```toml
[service]
name = "collector"
version = "0.1.0"        # type defaults to backend (gRPC on 50051)

[service.http]
port = 8081              # extra HTTP port, rendered alongside grpc
health_path = "/health" # optional httpGet probe on :8081
```

This renders a Service with both `grpc` and `http` ports, a Deployment with both container ports plus the HTTP probe, and (under Cilium) caller ingress rules for both.

`tonin helm generate` is offline — it only writes Helm templates. Anything that talks to a cluster (`diff`, `upgrade`, `uninstall`) needs a reachable Kubernetes API. There is no embedded cluster; bring your own.

**On a dev machine,** any of the following works:

| Option | Notes |
|---|---|
| [Rancher Desktop](https://rancherdesktop.io) | Recommended. Ships containerd, k3s, and a working kubeconfig out of the box. Mesh-compatible. |
| [OrbStack](https://orbstack.dev) (macOS) | Lightweight; enable Kubernetes mode in settings. |
| Docker Desktop | Enable Kubernetes in settings. Heavier than the above. |
| [kind](https://kind.sigs.k8s.io) | `kind create cluster` — fastest for CI / scripted setup. |
| [k3d](https://k3d.io) | `k3d cluster create` — k3s in Docker; good for multi-node testing. |
| [minikube](https://minikube.sigs.k8s.io) | Mature; many drivers. |

Sanity check after install:

```bash
kubectl version --short    # both Client and Server should print
kubectl get nodes          # at least one Ready node
```

**For a real deployment**, any conformant managed Kubernetes works: GKE,
EKS, AKS, DOKS, Civo, Linode LKE, or self-managed (RKE2, Talos, kubeadm,
OpenShift). The framework doesn't depend on cloud-provider-specific
extensions.

**Mesh install runs separately.** If `[deploy].mesh` is set to anything
other than `none`, install the mesh on the cluster *before* `tonin k8s
apply`:

```bash
cilium install                   # Cilium
linkerd install | kubectl apply -f -    # Linkerd
istioctl install --set profile=demo     # Istio
```

`tonin helm upgrade` will succeed without the mesh installed, but the
mesh-specific overlays (CiliumNetworkPolicy, PeerAuthentication, etc.)
will sit dormant until the mesh CRDs exist.

## Workloads, storage, ports, probes and resources

### Deployment or StatefulSet

```toml
[deploy]
kind                             = "statefulset"   # "deployment" (default) | "statefulset"
pod_management_policy            = "Parallel"      # StatefulSet only: "OrderedReady" (default) | "Parallel"
termination_grace_period_seconds = 60              # also per env: [deploy.<env>]
replicas                         = 2

[deploy.strategy]          # Deployment: type = "RollingUpdate" | "Recreate", max_surge, max_unavailable
max_surge       = 1        #   (integers or "25%")
max_unavailable = 0

[deploy.prd.strategy]      # StatefulSet: type = "RollingUpdate" | "OnDelete", partition
partition = 1              # stage a rollout: only pods with ordinal >= 1 update
```

A StatefulSet gets `serviceName: <name>-headless` and a headless Service of that name (stable
pod DNS `<name>-<ordinal>.<name>-headless.<ns>.svc`), the regular ClusterIP Service, and one
PersistentVolumeClaim per pod for each `pvc` volume. `podManagementPolicy`, `serviceName` and
`volumeClaimTemplates` are immutable on a StatefulSet: changing them later needs the StatefulSet
deleted (`kubectl delete sts <name> --cascade=orphan`) before the upgrade.

`[deploy.<env>.strategy]` overrides `[deploy.strategy]` field by field. The overlay is rendered
into `values-<env>.yaml` as `workload.strategy`, under `strategy` (Deployment) or `updateStrategy`
(StatefulSet).

### Autoscaling

```toml
[autoscale]
min_replicas = 2    # default: [deploy] replicas
max_replicas = 8

[autoscale.dev]     # per env: enabled, min_replicas, max_replicas
enabled = false
```

An HPA (CPU, 70 %) is rendered when the environment's range is wider than one replica count.
`[autoscale]` on a StatefulSet is an error (it keeps a fixed replica count); so is
`min_replicas > max_replicas`.

### Volumes

```toml
[[volumes]]
name       = "data"
type       = "pvc"                 # StatefulSet only: one claim per pod
mount_path = "/home/nonroot/data"
size       = "8Gi"
# storage_class = "fast"           # default: the cluster's default StorageClass (platform contract)
# access_modes  = ["ReadWriteOnce"]

[[volumes]]
name       = "parquet-cache"
type       = "empty-dir"           # pod-lifetime scratch space
mount_path = "/home/nonroot/data/parquet-cache"
size_limit = "2Gi"                 # optional; the kubelet evicts the pod above it
# medium   = "Memory"              # tmpfs (counts against the memory limit)
# read_only = true
```

Volumes are mounted in the server container in mount-path order (a volume nested inside another
mount works) and in the migration initContainer; the migration Job, which is not part of the
StatefulSet, gets the `empty-dir` volumes only. With a read-only root filesystem, every
directory the app writes to (`/tmp`, caches) needs one. Names starting with `pg-ca` are reserved.

### Extra ports and Services

```toml
[[ports]]
name    = "otlp-http"   # Kubernetes port name: <= 15 chars, lowercase, at least one letter
port    = 4318
service = "ingest"      # on Service <name>-ingest; omit to put it on the main Service

[[ports]]
name     = "query"
port     = 8081
service  = "internal"   # several ports may share one extra Service
# protocol = "TCP"      # "TCP" (default) | "UDP" | "SCTP"
```

Each port becomes a named `containerPort`, a Service port (`targetPort` by name), and is listed
in the chart's `extraPorts` values. Names `grpc`, `http`, `mcp`, `proxy` and the ports tonin
already uses (`[service].port`, `[service.http]`, MCP 50052, proxy 6565) are rejected.

### Probes

```toml
[service.health]
path           = "/health"         # default for both probes
port           = 8080
liveness_path  = "/health/live"
readiness_path = "/health/ready"

[service.health.startup]           # optional startupProbe; liveness/readiness wait for it
path              = "/health"      # default: the liveness path
period_seconds    = 10             # default 10
failure_threshold = 30             # default 30 (up to 5 minutes to start)
```

A gRPC-only service is probed with native `grpc:` probes; path fields are then an error (add
`[service.http]` to probe an HTTP endpoint). `[service.health.startup]` without fields still adds
a grpc startupProbe.

### Resources

```toml
[resources]          # shorthand: requests = limits
cpu    = "100m"
memory = "128Mi"
```

or, apart, with the CPU limit left out (no CPU throttling) and ephemeral storage:

```toml
[resources.requests]
cpu               = "250m"
memory            = "512Mi"
ephemeral_storage = "1Gi"     # covers emptyDirs on node disk, logs and the writable layer

[resources.limits]
memory            = "512Mi"
ephemeral_storage = "2Gi"
```

`requests.cpu` and `requests.memory` are required; limits are exactly what you write. Using both
forms at once is an error. Quantities are validated (`"500m"`, `"256Mi"`, `"8Gi"`).

### Command and language

```toml
[service]
language = "node"                     # a plain Node.js server (e.g. Next.js standalone)
command  = ["node"]                   # optional; [] or omitted = the image's ENTRYPOINT/CMD
args     = ["apps/shell/server.js"]   # optional
```

Rust services default to `command = ["/usr/local/bin/<name>"]` and get `RUST_LOG`; every other
language runs the image as built. `language = "node"` also means no tonin SDK in the image: no
MCP sidecar and no outbound proxy sidecar, even with `[depends_on]` (use `"typescript"` for a
tonin TypeScript service, which gets both).

## Commands

```bash
# Render Helm chart to ./chart/: values.yaml + one values-<env>.yaml per --env
tonin helm generate --env dev --env prd

# Fail (exit 1) if ./chart/ differs from what generate would write
tonin helm check --env dev --env prd

# Render templates locally to stdout (dry-run)
tonin helm template --env dev

# Show the diff vs. what's currently deployed (requires helm-diff plugin)
tonin helm diff --env prd

# Deploy to the cluster (helm upgrade --install)
tonin helm upgrade --env prd
```

`generate` also deletes chart files that an earlier tonin version generated and
this one no longer ships (a fixed list in the CLI), so a stale template never
keeps rendering; `helm check` reports such files as `obsolete`. Files tonin
never generated, such as your own templates under `chart/templates/`, are never
touched.

`--env` is required by `helm generate`, `helm check`, `affected`, `release` and
`deploy`; there is no default environment, so a chart or lock is never written
for an environment you did not name. The `tonin helm` cluster commands
(`upgrade`, `template`, `diff`, `status`, `rollback`, `history`, `uninstall`)
also accept `TONIN_ENV` in place of `--env`.

## Multi-env overlays

The base block is the default. Nested `[<section>.<env>]` blocks override fields per environment without duplicating the rest.

The chart splits along the same line:

- `values.yaml` holds only environment-independent settings (image repository,
  ports, probes, resources, security context, migrations command). Every key
  that can differ per environment (replicas, autoscaling, mesh, MCP mode,
  ingress, `[env]`, `[database]`/`[cache]`, secret keys, callers and
  dependencies) has a neutral default there (`replicaCount: 1`, `mesh: "none"`,
  `env: {}`, …), so one environment's settings never leak into another.
- `values-<env>.yaml` sets all of those keys for that environment.

Always render with both files: `-f values.yaml -f values-<env>.yaml`
(`tonin helm upgrade|template|diff` and `tonin deploy` do this for you).

```toml
[database]
engine = "postgres"

[database.dev]
url = "postgresql://postgres:postgres@postgres.shared-dev.svc.cluster.local:5432/app_dev"

[database.prd]
tls = "verify-full"
```

```bash
tonin helm generate --env dev --env prd   # dev: literal URL; prd: contract Secret + CA mount
```

The same pattern works for `[cache.<env>]`, `[migrations.<env>] mode`, and for the `[deploy]` fields that differ between environments (replicas, namespace, mesh, `mcp_sidecar`, `expose`).

### Per-environment namespaces and dependencies

Namespaces in `[deploy]` and `[depends_on]` resolve per environment. The
`{env}` placeholder is substituted with the environment being rendered, so a
single declaration covers every environment:

```toml
[deploy]
namespace = "myapp-{env}"          # → myapp-dev / myapp-prd

[depends_on]
users-service     = "myapp-{env}"      # egress target follows the same convention
inventory-service = "inventory-{env}"
```

When a dependency doesn't follow the `<base>-<env>` convention, use the table
form — a shorthand string *or* a table is accepted per entry (the same
ergonomics as a `Cargo.toml` dependency):

```toml
[depends_on]
users-service     = "myapp-{env}"                                                          # shorthand
inventory-service = { namespace = "inventory-{env}", prod = "inventory-shared" }           # prod overrides the convention
audit-sink        = { namespace = "security-{env}", envs = ["prod"] }                      # only egresses in prod
billing           = { namespace = "@inherit" }                                             # namespace set at deploy time; omitted from the chart
otel-gateway      = { namespace = "observability-{env}", port = 4317 }                     # non-gRPC / non-default listen port
```

Resolution rules for an environment `E`:

- A per-env override key (`prod = "..."`) wins over the default `namespace`.
- `envs = [...]` restricts the dependency to those environments; elsewhere it is dropped.
- `@inherit` omits the entry from the rendered policy — supply it at deploy time (`--set-json`) or via GitOps.
- If an active dependency has no namespace for `E`, or a `{...}` placeholder is left unresolved, generation **fails** — there is no silent fallback to a base value, which is what previously let a dev namespace leak into a prod chart.
- `port` declares the dependency's listen port; the generated CiliumNetworkPolicy egress rule allows exactly that port. Omitted (and for the shorthand string form) it defaults to `50051`, the tonin gRPC convention — so existing files render the same egress rule as before.

Both the shorthand string form and literal namespaces (no `{env}`) keep working unchanged.

### Plain environment variables (`[env]`)

Runtime configuration that is neither stateful (`[database]`/`[cache]` emit
`DATABASE_URL`/`REDIS_URL` for you) nor secret (`[secrets]` emits
`secretKeyRef` env) goes in the optional `[env]` table — plain string env vars
injected verbatim into the server container:

```toml
[env]
IDENTITY_GRPC_URL = "http://identity.myapp-{env}.svc.cluster.local:50051"
LOG_FORMAT        = "json"

[env.dev]
LOG_FORMAT = "pretty"        # merged over the base for dev only
```

- `[env.<env>]` overlays merge over the base entries for that environment
  (overlay wins per key; other base keys survive).
- `{env}` in values substitutes the environment being rendered, same as
  namespaces. Other `{...}` tokens pass through verbatim — env values may
  legitimately contain braces.
- Values must be strings (`PORT = "8080"`, not `PORT = 8080`).
- Keys that collide with env vars tonin already emits (`DATABASE_URL` /
  `REDIS_URL` from `[database]`/`[cache]`, or secret-sourced keys) fail generation — duplicate
  container env names are last-one-wins in Kubernetes, so a collision would
  silently override the resolved value.

The table renders into the chart's `env:` map in each `values-<env>.yaml`
(the base `values.yaml` keeps `env: {}`), so it survives regeneration — unlike hand-edits to the
`extraEnv: []` deploy-time escape hatch, which `tonin helm generate`
intentionally leaves empty.

## Fleet deploys from a lock (`tonin deploy`)

`tonin release --env <ENV>` records a service's version and digest-pinned image
in `environments/<ENV>/lock.toml`; `tonin deploy --env <ENV>` reconciles the
cluster to that lock in dependency-wave order:

```bash
tonin deploy --env prd                              # every service in the lock
tonin deploy --env prd --service identity           # only these services (repeatable)
tonin deploy --env prd --affected --base v0.14.0    # only services affected since a ref
tonin deploy --env prd --dry-run                    # print the helm commands, change nothing
```

For each service it runs, with the chart at `<service dir>/chart` (absolute):

```bash
helm upgrade --install <service> <service dir>/chart \
  -f values.yaml -f values-<ENV>.yaml --namespace <namespace> \
  --set-string image.repository=<repo> --set-string image.digest=sha256:… \
  --wait --timeout <timeout> --atomic --cleanup-on-fail
```

- The release is named after the service (`[service].name`), the same release
  `tonin helm upgrade` manages.
- The image comes from the lock. A digest-pinned image sets `image.digest`, and
  the chart renders `repository@digest`; no mutable tag is set. `image.tag`
  is only set when the lock image has no digest.
- A service is skipped when its release already runs the lock image (compared
  by image, not chart version); `--converge` upgrades every selected service.
- `--affected` uses the same computation as `tonin affected --base <ref>`.

## How it fits together

```mermaid
flowchart LR
    A[tonin.toml] --> B[planner<br/>parses every section<br/>+ --env overlay]
    B --> C[renderer<br/>pulls templates from<br/>embedded templates]
    C --> D[./chart/templates/]
    D --> E[tonin helm upgrade]
    E --> F[Kubernetes cluster]
```

The planner is the only thing that decides *what* gets rendered. The templates decide *how*. Add a field to `tonin.toml`, plumb it through the planner, and every service inherits it on the next `generate`.

## See also

- [01-principles.md](01-principles.md) — why `tonin.toml` is the single source of truth
- [13-service-mesh.md](13-service-mesh.md) — the mesh overlays rendered under `mesh/<engine>/`
- [08-database.md](08-database.md) — what `[database]` configures at runtime
- [07-cache.md](07-cache.md) — what `[cache]` configures at runtime

Full reference: [examples/greeter](https://github.com/Rushit/tonin/tree/main/examples/greeter).

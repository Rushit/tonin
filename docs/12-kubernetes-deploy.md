# Kubernetes deploy

One `tonin.toml` renders every manifest your service needs to ship.

## What you get

- **Every manifest a real service needs**, generated from a single TOML file:
  - `Deployment`, `Service`, `HorizontalPodAutoscaler`
  - `Ingress` when you ask for one
  - `DATABASE_URL` / `REDIS_URL` from the platform's Secrets, plus egress to the database/cache pods, when `[database]` / `[cache]` is set (tonin references databases and caches, it does not run them: see [08-database.md](08-database.md))
  - A migration initContainer or pre-install/pre-upgrade hook `Job` when `[migrations]` is set
  - A `ServiceAccount` per service, `imagePullSecrets`, node placement, a `PodDisruptionBudget`, topology spread and `CronJob`s (`[deploy] image_pull_secrets`, `[service_account]`, `[placement]`, `[pdb]`, `[[cronjobs]]`; see [Pods](#pods-pull-secrets-serviceaccount-placement-disruption-cronjobs))
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
| `serviceaccount.yaml` | always (the service's ServiceAccount; plus a hook `<service>-migrate` ServiceAccount when migrations run as a Job) |
| `pdb.yaml` | `[pdb]` enabled for the environment and more than one replica |
| `cronjob.yaml` | one `CronJob` per `[[cronjobs]]` entry active in the environment |
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
mount works) and in the migration initContainer. The migration Job and the `[[cronjobs]]`, which
are not part of the StatefulSet, get the `empty-dir` volumes only (each pod its own, empty), never
the StatefulSet's `pvc` claims: a CronJob cannot read or write a replica's persistent data. With a
read-only root filesystem, every directory the app writes to (`/tmp`, caches) needs one. Names
starting with `pg-ca` are reserved.

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

## Strict schema

Since 0.15 every key in `tonin.toml` must be known: a misspelt key fails the load instead of
being ignored. The error names the line, the key, the table and the keys allowed there:

```text
tonin.toml: line 7, column 1: unknown key `deploy.replica` in [deploy]; allowed keys:
replicas, namespace, mesh, mcp_sidecar, expose, kind, pod_management_policy,
termination_grace_period_seconds, strategy, image_pull_secrets, [deploy.<env>] overlays
```

- Sections that take per-environment overlays (`[deploy]`, `[autoscale]`, `[database]`,
  `[cache]`, `[migrations]`, `[secrets]`, `[placement]`, `[pdb]`, `[egress]`, `[env]`,
  `[callers]`) accept any other **table** as an `[<section>.<env>]` overlay. Its keys are
  checked against the overlay's own list, and its name must look like an environment
  (lowercase letters, digits, `-`).
- `tonin helm generate` warns about overlays naming an environment it is not rendering
  (`[deploy.prod]` when you generate `--env dev --env prd`): such an overlay never applies.
- Values are type-checked as before (`replicas = "2"` fails), and enumerated values are
  checked too: `[database] engine` must be one of `postgres`, `mysql`, `sqlite`, `clickhouse`,
  `none`; `[cache] engine` one of `redis`, `none` (0.14 silently treated an unknown engine as
  `none`).
- Free-form tables stay free-form: `[security.pod]` / `[security.container]`,
  `labels`, `node_selector`, `[secrets] map` and `[[cronjobs]] env`.
- `[placement] tolerations` and `topology_spread` entries are checked the same way (since
  0.16, with the line of the entry): their keys may be `snake_case` or `camelCase`, and a
  spread constraint without `max_skew`, `topology_key` or `when_unsatisfiable` names the
  missing key.
- `[storage] kind` (written by `tonin new --with-storage`) is accepted; the chart does not
  read it.

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
  ingress, `[env]`, `[database]`/`[cache]`, secret keys and `secrets.create`,
  callers and dependencies, pull secrets, placement, PDB, CronJobs) has a
  neutral default there (`replicaCount: 1`, `mesh: "none"`,
  `env: {}`, …), so one environment's settings never leak into another.
- `values-<env>.yaml` sets all of those keys for that environment.

Always render with both files: `-f values.yaml -f values-<env>.yaml`
(`tonin helm upgrade|template|diff` and `tonin deploy` do this for you).

```toml
[database]
engine = "postgres"

[database.dev]
url       = "postgresql://postgres:postgres@postgres.shared-dev.svc.cluster.local:5432/app_dev"
namespace = "shared-dev"                              # egress to the shared instance
labels    = { "app.kubernetes.io/name" = "postgres" }

[database.prd]
tls = "verify-full"
```

```bash
tonin helm generate --env dev --env prd   # dev: literal URL; prd: contract Secret + CA mount
```

The same pattern works for `[cache.<env>]`, `[migrations.<env>] mode`, `[secrets.<env>] create`, `[placement.<env>]`, `[pdb.<env>]`, and for the `[deploy]` fields that differ between environments (replicas, namespace, mesh, `mcp_sidecar`, `expose`, `image_pull_secrets`).

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
telemetry         = { namespace = "telemetry-{env}", ports = [8080, 8081, 8082] }             # several ports
```

Resolution rules for an environment `E`:

- A per-env override key (`prod = "..."`) wins over the default `namespace`.
- `envs = [...]` restricts the dependency to those environments; elsewhere it is dropped.
- `@inherit` omits the entry from the rendered policy — supply it at deploy time (`--set-json`) or via GitOps.
- If an active dependency has no namespace for `E`, or a `{...}` placeholder is left unresolved, generation **fails** — there is no silent fallback to a base value, which is what previously let a dev namespace leak into a prod chart.
- `port` declares the dependency's listen port, `ports` a list of them (give one or the other); the generated CiliumNetworkPolicy egress rule allows exactly those ports. Omitted (and for the shorthand string form) it defaults to `50051`, the tonin gRPC convention — so existing files render the same egress rule as before.

Both the shorthand string form and literal namespaces (no `{env}`) keep working unchanged.

### Network policy (`[callers]`, `[egress]`)

Under `mesh = "cilium"` every service gets one `CiliumNetworkPolicy`
(`<svc>-ingress`) that is default-deny in both directions and allows:

| Direction | Allowed | From |
|---|---|---|
| ingress | each caller, on its ports | `[callers]` |
| egress | kube-dns (53/UDP+TCP) | always |
| egress | the otel-collector (`app=otel-collector` in `observability`), 4317 | always (the collector is push-only; it never connects to pods) |
| egress | other tonin services, on their ports | `[depends_on]` |
| egress | databases and caches by contract pod labels | `[database]` / `[cache]` |
| egress | DNS names, IP ranges, the kube-apiserver | `[egress]` |

With no callers the policy still denies all ingress (it renders an empty
`ingress: [{}]` rule). The migration Job (`[migrations] mode = "job"`) is
excluded from this policy and has its own hook policy: DNS, the otel-collector
and the datastores only, no `[depends_on]` or `[egress]`.

#### `[callers]`

```toml
[callers]
# Shorthand: the tonin service `ui-shell` in that namespace, default ports.
ui-shell = "ui-{env}"

# Caller table: pods matched by labels, in `namespace` (omit it for the
# service's own namespace), on these ports only.
cloudflared = { namespace = "edge", labels = { "app.kubernetes.io/name" = "cloudflared" }, ports = [4318] }

# Caller table for a tonin service: `service` names it when the key differs.
billing = { service = "billing-api", namespace = "billing-{env}", ports = [7001] }

# Env overlay: entries (same two forms) added or replaced in prd only.
[callers.prd]
ui-shell = "ui"
```

- A caller table has the reserved keys `namespace`, `labels`, `service` and
  `ports`; anything else is an error that lists them. `labels` and `service`
  are exclusive. A service caller needs `namespace`.
- A `[callers]` sub-table with **none** of the reserved keys is an
  environment overlay, `[callers.<env>]`; its keys are callers. So a caller
  can't be named `namespace`, `labels`, `service` or `ports`, and
  `[callers.cloudflared]` with `namespace = …` is a caller, not an overlay.
  Overlays don't nest; overlays of other environments are still validated.
- `ports` (non-empty, 1-65535) replaces the default set: the main port, the
  `[service.http]` port, and the MCP port when MCP is on.
- Service callers are matched by `service.identity: <name>.<namespace>`;
  label callers by their labels plus the namespace label. A service caller
  or `[depends_on]` service in another namespace also gets the namespace
  label (`k8s:io.kubernetes.pod.namespace`): Cilium scopes a selector
  without it to the policy's own namespace.
- Only service callers take part in `tonin affected` propagation (and
  workspace loading adds service callers derived from other services'
  `[depends_on]`; an explicit entry for the same service wins).

#### `[egress]`

```toml
[egress]
fqdns = [
  { name = "api.stripe.com", ports = [443] },   # Cilium toFQDNs matchName
  { pattern = "*.example.com", ports = [443] },      # matchPattern
]
cidrs = [{ cidr = "10.0.0.0/8", except = ["10.96.0.0/12"], ports = [5432] }]  # toCIDRSet
kube_apiserver = true                           # toEntities: [kube-apiserver], ports 443 and 6443

[egress.prd]                                    # per env: each key set here replaces the base value
fqdns = [{ name = "s3.example-region.example.com", ports = [443] }]
```

- Every FQDN and CIDR entry needs `ports`. A FQDN entry has exactly one of
  `name` (letters, digits, `-`, `.`) and `pattern` (the same plus `*`).
  CIDRs are IPv4 or IPv6 `ip/prefix`.
- When `fqdns` is non-empty, the kube-dns rule gains
  `rules: dns: [{ matchPattern: "*" }]`. `toFQDNs` only works when Cilium's
  DNS proxy sees the pod's lookups, so the rule is required; it does not
  restrict which names resolve. Services without FQDN egress don't get it,
  so their DNS doesn't go through the proxy (and they don't need Cilium's
  L7 proxy).

In `values-<env>.yaml` this is the `networkPolicy` key: `callers` (`name`,
`namespace`, optional `labels` and `ports`), `dependsOn` (`name`, `namespace`,
`ports`) and `egress` (`fqdns`, `cidrs`, `kubeApiserver`).

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

## Pods: pull secrets, ServiceAccount, placement, disruption, CronJobs

Every pod tonin renders (the Deployment, the migration Job, CronJobs) gets the
same pod-level settings from one chart helper (`service.podSpecCommon`): the
ServiceAccount, `automountServiceAccountToken`, `imagePullSecrets`, the pod
`securityContext` from `[security.pod]`, `nodeSelector` and `tolerations`.
Containers get `[security.container]`.

```toml
[deploy]
image_pull_secrets = []                       # Secret names; per env in [deploy.<env>]

[deploy.prd]
image_pull_secrets = ["ghcr-read"]

[placement.prd]                               # base [placement] + per-env overlays
node_selector = { "example.com/role" = "app" }
tolerations   = [{ key = "example.com/db", operator = "Equal", value = "identity", effect = "NoSchedule" }]
# topology_spread = false                     # default: hostname spread, see below
# topology_spread = [{ max_skew = 1, topology_key = "topology.kubernetes.io/zone", when_unsatisfiable = "DoNotSchedule" }]

[pdb.prd]                                     # [pdb] alone = max_unavailable 1
min_available = 1                             # or max_unavailable = 1 / "50%"

[service_account]
automount_token = false                       # default
tokens = [                                    # projected, audience-bound, kubelet-rotated
  { audience = "identity" },                  # → /var/run/secrets/tokens/identity, 3600 s
  { audience = "telemetry", path = "/var/run/secrets/tokens/telemetry", expiration_seconds = 3600 },
]
# token = { audience = "identity" }           # shorthand for a single token

[secrets.prd]
create = false                                # <service>-secrets comes from a SealedSecret

[[cronjobs]]
name     = "purge-expired"                    # CronJob <service>-purge-expired
schedule = "0 2 * * *"
command  = ["/usr/local/bin/identity-purge-expired"]

[[cronjobs]]
name                      = "close-impersonation"
schedule                  = "0 * * * *"
args                      = ["close-impersonation"]   # keeps the service command
concurrency_policy        = "Forbid"          # default; Allow | Replace
successful_jobs_history_limit = 3             # default 3
failed_jobs_history_limit     = 3             # default 3
starting_deadline_seconds = 300               # optional
backoff_limit             = 3                 # default 3
resources                 = { cpu = "50m", memory = "64Mi" }  # default: the service's
env                       = { LOG_FORMAT = "json" }         # wins over [env]
envs                      = ["prd"]           # default: every environment
```

- **Pull secrets** name existing Secrets (the platform provides them; tonin
  never renders registry credentials). `[deploy.<env>] image_pull_secrets`
  replaces the base list.
- **ServiceAccount**: named after the service, so a caller is
  `system:serviceaccount:<namespace>:<service>`. No API token is mounted unless
  `automount_token = true`. Each token in `tokens` is a projected
  `serviceAccountToken` (minimum `expiration_seconds` 600); tokens in the same
  directory share one projected volume mounted read-only into the server
  container (never `subPath`, which would stop rotation).
- **Migration Job ServiceAccount**: the Job is a pre-install/pre-upgrade hook,
  so on a first install it runs before the chart's own resources exist. It runs
  as `<service>-migrate`, a hook ServiceAccount at weight `-10` (before the hook
  network policy at `-5` and the Job at `0`), deleted with the Job, and never
  mounts an API token. The service's identity is never shared with the Job.
- **Placement**: each field of `[placement.<env>]` replaces the base field.
  Tolerations and spread constraints are checked by the strict schema (see
  above; `snake_case` or `camelCase` keys).
- **Topology spread** is on by default: `maxSkew: 1` over
  `kubernetes.io/hostname`, `ScheduleAnyway`, selecting the service's pods.
  Constraints without a `label_selector` get the service's selector labels.
  It is rendered only on the service's own pods, and only when they run more
  than one replica (`autoscaling.minReplicas` with an HPA).
- **PDB**: present means enabled (`enabled = false` turns it off per env).
  Skipped automatically with one replica. `min_available` at or above the
  replica count, or `max_unavailable = 0`, fails generation: such a budget
  blocks every node drain. The PDB sets `unhealthyPodEvictionPolicy:
  AlwaysAllow`, so a crash-looping pod never blocks node maintenance.
- **`secrets.create`** per env: `false` references a `<service>-secrets`
  Secret managed elsewhere (sealed secrets, External Secrets).
- **CronJobs** run the service image with the service's env, `[secrets]` keys,
  datastores (env and CA mount), security context, ServiceAccount, pull
  secrets and placement. `command` replaces the service command; with only
  `args`, the service command runs with those args (one of the two is
  required). Pods carry `service.identity`, so the service's network policy
  (egress to its datastores and dependencies) applies, but not the Service
  selector label. Names are DNS labels and `<service>-<name>` is at most 52
  characters.

## Fleet deploys from a lock (`tonin deploy`)

`tonin release --env <ENV>` records a service's version and digest-pinned image
in `environments/<ENV>/lock.toml`. The lock is edited in place: `[services.*]`
tables stay sorted by name, only the released service's table changes, and
re-releasing an identical entry leaves the file untouched (the command prints
`unchanged`), so there is nothing to commit. Only `[services.<name>]` tables
are supported; entries written as dotted keys or inline tables are normalised
to that form the next time the file changes. A lock that exists but can't be
parsed (for example, an entry missing `git_sha`) makes `tonin release` fail
instead of starting a new lock.

`tonin deploy --env <ENV>` reconciles the cluster to that lock in
dependency-wave order:

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
  --set-string image.version=<lock version> \
  --wait --timeout <timeout> --atomic --cleanup-on-fail
```

- The release is named after the service (`[service].name`), the same release
  `tonin helm upgrade` manages.
- The image comes from the lock. A digest-pinned image sets `image.digest`, and
  the chart renders `repository@digest`; no mutable tag is set. `image.tag`
  is only set when the lock image has no digest.
- `image.version` is the lock's version. The `app.kubernetes.io/version` label and
  the OTEL `service.version` read it (falling back to `image.tag` when it is
  empty, as with `tonin helm upgrade`), so a digest-pinned release reports the
  version it runs, not the version the chart was generated at.
- A service is skipped when its release already runs the lock image (compared
  by image, not chart version); `--converge` upgrades every selected service.
- `--affected` uses the same computation as `tonin affected --env <ENV> --base <ref>`.

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

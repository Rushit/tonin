# Migrating from tonin 0.14 to 0.15

tonin 0.15 makes generated charts production-ready: databases and caches become references to
a platform contract, workloads gain StatefulSets, volumes and extra ports, network policy gains
label callers and egress rules, every pod gets its own ServiceAccount and pull secrets, and
`tonin.toml` is validated strictly. Most changes are additive; this guide lists the ones you
must act on, in the order you will hit them.

## 1. Upgrade the CLI and pass `--env` everywhere

```bash
tonin upgrade            # or install 0.15.x
tonin helm generate --env dev --env prd     # was: tonin helm generate
tonin helm check    --env dev --env prd
tonin affected      --env prd --base origin/main
tonin release       --env prd ...
```

`--env` is required by `helm generate`, `helm check`, `affected` and `release`, and by the
`tonin helm` proxy commands unless `TONIN_ENV` is set. There is no default environment.

## 2. Fix what the strict schema rejects

Load each `tonin.toml` (`tonin helm generate --env …`). Every unknown key is now an error with
its line and the keys allowed there, e.g.

```text
tonin.toml: line 7, column 1: unknown key `deploy.replica` in [deploy]; allowed keys: replicas, …
```

The fields 0.15 removed fail with a pointer to the replacement:

| 0.14 | 0.15 |
| --- | --- |
| `[database] size`, `version` | gone: the platform layer sizes and versions the database |
| `[database.<env>] shared = true`, `[cache.<env>] shared = true` | remove; every database/cache is a contract reference. Keep a literal `url` under `[<section>.<env>]` for dev |
| `[database] name`, `[cache] name` | `secret` (the Secret with the uri) and `cluster` / `labels` (egress target) |
| base-table `url` | move it under `[database.<env>]` / `[cache.<env>]` |
| `[migrations] run_on = "init-container"` | `mode = "init-container"` (`"job"`, `"none"`; per env in `[migrations.<env>]`) |
| `run_on = "boot"` / `"manual"` | `mode = "none"` |
| `[database] engine = "<unknown>"` | one of `postgres`, `mysql`, `sqlite`, `clickhouse`, `none` (0.14 treated it as `none`) |

`helm generate` also warns about overlays for environments you are not generating, such as a
leftover `[deploy.prod]` when your environments are `dev` and `prd`.

## 3. Provide the platform contract

tonin no longer renders Postgres or Redis. In every namespace a service with `[database]` /
`[cache]` runs in, the platform (CNPG, a Redis manifest, a managed service, or your dev setup)
must provide:

| What | Default name | Override |
| --- | --- | --- |
| Secret with the connection uri (key `uri`) | `<svc>-pg-app` / `<svc>-cache` | `secret`, `key`, `cluster` |
| CA Secret (key `ca.crt`), only for `tls = "verify-full"` | `<svc>-pg-ca` | `ca_secret` |
| Pod labels egress is allowed to | `cnpg.io/cluster=<svc>-pg` / `app.kubernetes.io/name=<svc>-cache` | `labels`, `namespace`, `port` |
| A pull secret, if you set `image_pull_secrets` | e.g. `ghcr-read` | |
| A default StorageClass, for `pvc` volumes | | `storage_class` |

`generate` deletes the `db-*.yaml` and `cache-*.yaml` templates earlier versions rendered.

## 4. Review behaviour that changed without a config change

- **Network policy.** The otel-collector can no longer connect *to* your pods (it is push-only;
  egress to it on 4317 stays). A service with no `[callers]` gets a default-deny ingress rule.
  Callers that are not tonin services (cloudflared, an ingress controller) must be listed as
  label callers, or their traffic is dropped.
- **ServiceAccount.** Every pod runs as the chart's own ServiceAccount `<svc>` with
  `automountServiceAccountToken: false`. Set `[service_account] automount_token = true` if the
  app talks to the Kubernetes API; use `tokens` for audience-bound tokens.
- **Topology spread.** Services with more than one replica get a hostname spread
  (`ScheduleAnyway`); `[placement] topology_spread = false` turns it off.
- **`[autoscale] min_replicas`** now sets the HPA minimum (0.14 ignored it and used
  `[deploy] replicas`). `[autoscale.<env>] enabled = false` turns the HPA off per env.
- **Container command.** Unchanged for Rust (`/usr/local/bin/<name>`). Non-Rust services still
  run the image's CMD; a plain Node.js server should say `language = "node"` so it gets no MCP or
  outbound-proxy sidecar.
- **Release name.** `tonin deploy` uses `<service>` (not `<service>-<env>`); uninstall or migrate
  releases an older `tonin deploy` created.
- **Chart values.** `statefulEnv`, `database.*`, `cache.*` became `datastores`;
  `networkPolicy` callers/dependsOn changed shape; new keys are listed in
  [HELM.md](HELM.md#generated-service-charts-values-reference). Deploy-time `--set` flags or
  extra values files that used the old keys must be updated.

## 5. Replace hand-written templates with `tonin.toml`

Things that needed a custom template under `chart/templates/` in 0.14 are now generated:

| Hand-written in 0.14 | 0.15 `tonin.toml` |
| --- | --- |
| extra Service / port (`*-ports.yaml`) | `[[ports]]` with `service = "<suffix>"` |
| egress to extra dependency ports (`*-egress.yaml`) | `[depends_on] x = { namespace = "…", ports = [8080, 8081] }` |
| egress to SaaS APIs / object storage | `[egress] fqdns = [{ name = "api.stripe.com", ports = [443] }]`, `cidrs`, `kube_apiserver` |
| CronJobs | `[[cronjobs]]` |
| PDB, nodeSelector, tolerations | `[pdb]`, `[placement]` (per env) |
| writable dirs on a read-only root | `[[volumes]] type = "empty-dir"` |

Delete the hand-written files once the generated chart covers them; `generate` never deletes
files it did not write.

## Before and after

A gRPC service with Postgres and Redis, two callers, a hand-written CronJob and an HPA.

**0.14**

```toml
[service]
name    = "identity"
version = "0.3.7"

[deploy]
replicas  = 2
mesh      = "cilium"
namespace = "platform-{env}"

[deploy.dev]
replicas = 1

[resources]
cpu    = "100m"
memory = "128Mi"

[autoscale]
min_replicas = 2      # ignored by 0.14
max_replicas = 8

[database]
engine = "postgres"

[database.dev]
shared = true
url    = "postgresql://postgres:postgres@postgres.shared-dev.svc.cluster.local:5432/identity_dev"

[cache]
engine = "redis"

[cache.dev]
shared = true
url    = "redis://redis.shared-dev.svc.cluster.local:6379"

[secrets]
required = ["JWT_SECRET"]

[migrations]
tool    = "custom"
run_on  = "init-container"
command = ["/usr/local/bin/identity", "migrate"]

[callers]
api      = "platform-{env}"
ingestor = "ingest-{env}"
```

**0.15**

```toml
[service]
name    = "identity"
version = "0.3.7"

[deploy]
replicas           = 2
mesh               = "cilium"
namespace          = "platform-{env}"
image_pull_secrets = ["ghcr-read"]

[deploy.dev]
replicas = 1

[resources.requests]          # CPU limit left out
cpu    = "100m"
memory = "256Mi"

[resources.limits]
memory = "256Mi"

[autoscale]
min_replicas = 2
max_replicas = 8

[autoscale.dev]
enabled = false               # dev: one fixed replica

[placement.prd]
node_selector = { "node-role" = "app" }

[pdb]                         # maxUnavailable 1; skipped with one replica

[database]
engine = "postgres"           # prd: DATABASE_URL from Secret identity-pg-app / uri

[database.prd]
tls = "verify-full"           # mounts identity-pg-ca / ca.crt

[database.dev]
url = "postgresql://postgres:postgres@postgres.shared-dev.svc.cluster.local:5432/identity_dev"

[cache]
engine = "redis"              # prd: REDIS_URL from Secret identity-cache / uri

[cache.dev]
url = "redis://redis.shared-dev.svc.cluster.local:6379"

[secrets]
required = ["JWT_SECRET"]

[secrets.prd]
create = false                # prd Secret comes from Sealed Secrets / ExternalSecrets

[migrations]
tool    = "custom"
mode    = "init-container"
command = ["/usr/local/bin/identity", "migrate"]

[migrations.prd]
mode = "job"                  # pre-install/pre-upgrade hook Job

[callers]
api      = { namespace = "platform-{env}", ports = [50051] }
ingestor = "ingest-{env}"

[[cronjobs]]                  # replaces chart/templates/cronjob-purge-expired.yaml
name     = "purge-expired"
schedule = "0 2 * * *"
args     = ["purge-expired"]
```

Then regenerate and review the rendered chart per environment:

```bash
tonin helm generate --env dev --env prd
tonin helm template --env prd | less
git add chart && git commit -m "chore: tonin 0.15 chart"
```

# Cilium mesh templates

Rendered when `mesh = "cilium"` in `tonin.toml`.

## Files

| File | Per-service? | Purpose |
| ---- | ------------ | ------- |
| `pod-annotations.yaml.tmpl` | yes | Adds `io.cilium/encryption: enabled` so pod-to-pod traffic is WireGuard-encrypted by the kernel. |
| `networkpolicy.yaml.tmpl`   | yes | Renders a `CiliumNetworkPolicy` with default-deny ingress and explicit allows derived from `[depends_on]`. |

## Cluster prerequisites

- Cilium installed as the CNI (`--set encryption.enabled=true --set encryption.type=wireguard`).
- Cilium ≥ 1.14 for service-mesh CRDs.
- For multi-cluster: `cilium clustermesh enable` on each cluster, then `cilium clustermesh connect`.

## How identity works

micro labels every pod with `service.identity: <svc>.<ns>`. CiliumNetworkPolicy selects on that label
rather than `app`, so policies survive Deployment renames and naming collisions across namespaces.

## Switching away

Set `mesh = "istio"` (or `linkerd` / `none`) in `tonin.toml` and re-run `tonin generate`.
Only the contents of `k8s/mesh/` change — the Deployment, Service, and HPA stay identical.

# sycophant-quickstart

One-command hobbyist install. Bundles every cluster-level prereq for
running sycophant on a fresh k3d (or kind, or laptop minikube) cluster.

## What's inside

| Subchart | Purpose |
|---|---|
| `kyverno-crds` | Sibling CRDs chart; CRD definitions survive `helm uninstall` |
| `cilium` (upstream) | CNI with FQDN egress policy (load-bearing for chamber egress) |
| `kyverno` (upstream) | Admission policies, `generate` rules for tenant RBAC |
| `sycophant-gvisor` | gVisor RuntimeClass definition |
| `sycophant-cluster` | sycophant CRDs + cluster-scoped RBAC + Kyverno policies |

`sycophant-tenant` is NOT bundled — that's per-workspace and operators
install separately, possibly multiple times.

## Install

Requires k3d ≥ 5.0 (for the `@server:*` node-filter syntax below).

```sh
# 1. Create the k3d cluster with k3s's bundled flannel + network-policy
#    controller disabled. Cilium must be the only CNI for
#    CiliumNetworkPolicy (the chamber egress allowlist) to enforce.
k3d cluster create sycophant \
  --k3s-arg "--flannel-backend=none@server:*" \
  --k3s-arg "--disable-network-policy@server:*"

# 2. Install the quickstart bundle. --timeout=10m gives the Cilium
#    operator time to settle on slow Docker hosts.
cd charts/sycophant-quickstart
helm dependency update
helm install sycophant-quickstart . --wait --timeout=10m
```

Chambers require `runsc` on each node — see the "DevOps users" section below
for the install recipe (k3d only; production clusters use a node-installer
DaemonSet or pre-baked AMI).

Then install one or more tenants:

```sh
helm install my-tenant charts/sycophant-tenant -n my-tenant --create-namespace -f ...
```

## DevOps users

The quickstart is opinionated for hobbyists. DevOps deployments should
install each piece individually into their existing cluster:

- Skip `cilium` (you already have a CNI) — set `cilium.enabled=false`.
- Skip `kyverno` (you may have your own admission stack) — set `kyverno.enabled=false`.

With both flags off the quickstart reduces to `sycophant-cluster` +
`sycophant-gvisor` + the `kyverno-crds` chart — roughly equivalent to
a piece-by-piece install minus the upstream binaries.

For chamber pods to actually launch you also need `runsc` on each node.
See `scripts/e2e.sh:install_gvisor` for a reference recipe (download
binary, drop into `/usr/local/bin`, append containerd runtime block,
SIGHUP k3s).

## Uninstall

```sh
helm uninstall sycophant-quickstart --timeout=5m
```

`--wait` is intentionally omitted: on k3d single-node clusters, Cilium's
namespace teardown (cilium-secrets and chart-created namespaces) races with
its operator shutdown and namespaces sit in Terminating state for 15+
minutes. This is cosmetic — the helm release and Kyverno webhooks are gone
once `helm uninstall` returns. To force namespace cleanup, delete the cluster
itself (`k3d cluster delete sycophant`).

Kyverno's chart pre-delete hook scales its controllers to zero and deletes
its webhook configs before helm proceeds. If uninstall hangs (rare; see
[Kyverno issue #9551](https://github.com/kyverno/kyverno/issues/9551)),
recover with the [official procedure](https://kyverno.io/docs/installation/uninstallation/):

```sh
kubectl delete validatingwebhookconfiguration,mutatingwebhookconfiguration \
  -l webhook.kyverno.io/managed-by=kyverno
helm uninstall sycophant-quickstart
```

The sibling `kyverno-crds` chart's CRDs survive uninstall (helm's `crds/`
directory lifecycle), so operator-authored `ClusterPolicy` and sycophant
CRs are preserved across a `helm uninstall && helm install` rebuild.

## Why a sibling kyverno-crds chart

Bundling Kyverno's CRDs into this meta-chart would mean `helm uninstall
sycophant-quickstart` cascade-deletes every `ClusterPolicy` and
`PolicyException` in the cluster. The sibling `kyverno-crds` chart pins
the CRD lifecycle to a separate helm release so policies survive teardown.

Cilium's CRDs are installed by the cilium-operator at runtime (not via
helm), so they need no sibling chart — they're already outside helm's
ownership.

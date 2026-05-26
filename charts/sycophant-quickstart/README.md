# sycophant-quickstart

Cluster-scoped sycophant install. Ships the CRDs, the gVisor RuntimeClass,
and the cluster-wide RBAC + Kyverno policies that sycophant tenant
namespaces depend on.

## What's inside

| Subchart | Purpose |
|---|---|
| `sycophant-gvisor` | gVisor RuntimeClass definition |
| `sycophant-cluster` | sycophant CRDs + cluster-scoped RBAC + Kyverno ClusterPolicies + workspace VAP |

The `kyverno-crds` chart lives alongside but is NOT a dependency — it must be
installed BEFORE Kyverno (Kyverno's startup sanity check refuses to come Ready
without its CRDs). The hobbyist script installs it; DevOps users install it
manually before installing Kyverno.

`sycophant-tenant` is NOT bundled — that's per-workspace and operators install
separately, possibly multiple times.

## Prerequisites

Cilium, Kyverno, and the `kyverno-crds` sibling chart are **not** bundled.
The hobbyist install script does all three for you (see below). DevOps
operators are responsible for ensuring:

- **Cilium 1.19.x in `kube-system`** with the L7 DNS proxy enabled, kube-proxy
  replacement disabled, and a pod CIDR matching the cluster's pod range.
  CiliumNetworkPolicy is the load-bearing primitive for chamber egress
  allowlists — Calico or Flannel cannot substitute.
- **`kyverno-crds` chart installed as its own helm release** before Kyverno —
  Kyverno's startup sanity check refuses to come Ready if its CRDs are
  missing. Sycophant ships this chart at `charts/kyverno-crds/`; it places
  the CRDs in helm's install-only `crds/` directory so they survive
  Kyverno's helm uninstall (preserving operator-authored `ClusterPolicy` and
  `PolicyException` resources).
- **Kyverno 3.5.x in the `kyverno` namespace**, with `config.excludeKyvernoNamespace: true`
  (the chart default), `crds.install: false` (the kyverno-crds chart owns
  them), the cleanup + reports controllers disabled, and the
  `kyverno` namespace carrying `pod-security.kubernetes.io/enforce: restricted`.
- **`runsc` on each node** for gVisor isolation of workspace pods. On k3d:
  download the runsc binary into `/usr/local/bin`, append a `runsc` runtime
  block to `/etc/containerd/config.toml`, SIGHUP k3s. See
  `scripts/install-gvisor.sh` (or the matching block in `scripts/e2e.sh`)
  for a reference recipe. Production clusters use a node-installer
  DaemonSet or a pre-baked AMI.

Why Cilium/Kyverno aren't bundled: a Helm meta-chart can't install subcharts
into different namespaces — `dependencies[].namespace:` on Chart.yaml is
non-functional in Helm 4 (helm/helm#10905). Cilium must live in `kube-system`;
Kyverno must live in `kyverno`. The hobbyist script sequences three separate
helm installs to get each in its right place.

## Install — hobbyist

```sh
# 1. Create the k3d cluster with k3s's bundled flannel + network-policy
#    controller disabled. Cilium must be the only CNI.
k3d cluster create sycophant \
  --k3s-arg "--flannel-backend=none@server:*" \
  --k3s-arg "--disable-network-policy@server:*"

# 2. One-command install. Sequences Cilium → kube-system, Kyverno →
#    kyverno (with PSA labels), and this chart.
cargo run -p syco --release -- install
```

`syco install` lives in the workspace's `cli/` crate. It uses `helm upgrade
--install` everywhere, so re-running is idempotent.

Override the release namespace for `sycophant-quickstart` itself with
`--release-namespace foo` (or `RELEASE_NAMESPACE=foo`). Cilium and Kyverno
always land in their canonical namespaces regardless.

## Install — DevOps

Cilium and Kyverno are your responsibility. Once they're in place per the
prerequisites above:

```sh
cd charts/sycophant-quickstart
helm dependency update
helm install sycophant-quickstart . -n <your-ns> --wait --timeout=10m
```

Then install one or more tenants:

```sh
helm install my-tenant charts/sycophant-tenant -n my-tenant --create-namespace -f ...
```

## Uninstall

This chart's uninstall is straightforward — no Cilium/Kyverno entanglement:

```sh
helm uninstall sycophant-quickstart -n <release-ns> --timeout=5m
```

For the hobbyist who wants the cluster gone entirely:

```sh
k3d cluster delete sycophant
```

If you ran `syco install` and want to uninstall Cilium + Kyverno too, run
their respective `helm uninstall` commands:

```sh
helm uninstall kyverno -n kyverno
helm uninstall cilium -n kube-system
```

Kyverno's pre-delete hook scales its controllers to zero and deletes its
webhook configs before helm proceeds. If uninstall hangs (rare; see
[Kyverno issue #9551](https://github.com/kyverno/kyverno/issues/9551)),
recover with the [official procedure](https://kyverno.io/docs/installation/uninstallation/):

```sh
kubectl delete validatingwebhookconfiguration,mutatingwebhookconfiguration \
  -l webhook.kyverno.io/managed-by=kyverno
helm uninstall kyverno -n kyverno
```

The sibling `kyverno-crds` chart's CRDs survive uninstall (helm's `crds/`
directory lifecycle), so operator-authored `ClusterPolicy` and sycophant CRs
are preserved across a `helm uninstall && helm install` rebuild.

## Why a sibling kyverno-crds chart

Bundling Kyverno's CRDs into Kyverno's own chart would mean `helm uninstall
kyverno` cascade-deletes every `ClusterPolicy` and `PolicyException` in the
cluster. The sibling `kyverno-crds` chart pins the CRD lifecycle to a separate
helm release (this chart's release) so policies survive teardown.

Cilium's CRDs are installed by the cilium-operator at runtime (not via helm),
so they need no sibling chart — they're already outside helm's ownership.

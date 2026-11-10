# sycophant-quickstart

Cluster-scoped sycophant install. Ships the gVisor RuntimeClass and the
cluster-wide RBAC, Kyverno policies and admission policies that sycophant
tenant namespaces depend on.

## What's inside

| Subchart | Purpose |
|---|---|
| `sycophant-gvisor` | gVisor RuntimeClass definition |
| `sycophant-cluster` | cluster-scoped RBAC + Kyverno policies + ValidatingAdmissionPolicies |

The `kyverno-crds` chart lives alongside but is NOT a dependency — it must be
installed BEFORE Kyverno (Kyverno's startup sanity check refuses to come Ready
without its CRDs). `sycoadm setup` installs it; DevOps users install it
manually before installing Kyverno.

The per-tenant charts are NOT bundled; each tenant's components are provisioned separately.

## Prerequisites

Cilium, Kyverno, and the `kyverno-crds` sibling chart are **not** bundled.
`sycoadm setup` does all three for you (see below). DevOps
operators are responsible for ensuring:

- **Cilium 1.19.x in `kube-system`** with the L7 DNS proxy enabled, kube-proxy
  replacement disabled, and a pod CIDR matching the cluster's pod range.
  CiliumNetworkPolicy is the load-bearing primitive for toolset egress
  allowlists — Calico or Flannel cannot substitute.
- **`kyverno-crds` chart installed as its own helm release** before Kyverno —
  Kyverno's startup sanity check refuses to come Ready if its CRDs are
  missing. Sycophant ships this chart at `charts/kyverno-crds/`; it places
  the CRDs in helm's install-only `crds/` directory so they survive
  Kyverno's helm uninstall (preserving operator-authored policies and
  `PolicyException` resources).
- **Kyverno 3.9.x in the `kyverno` namespace**, with `config.excludeKyvernoNamespace: true`
  (the chart default), `crds.install: false` (the kyverno-crds chart owns
  them), the cleanup + reports controllers disabled, and the
  `kyverno` namespace carrying `pod-security.kubernetes.io/enforce: restricted`.
- **`runsc` on each node** provides the gVisor RuntimeClass used to isolate the
  capability-job pods that run agent-executed tool code. On k3d: copy
  `runsc`, `containerd-shim-runsc-v1` and the `gvisor-bin/` helpers from the
  gVisor release into `/usr/local/bin` on the node, add a `runsc` runtime to
  `/var/lib/rancher/k3s/agent/etc/containerd/config-v3.toml.tmpl`, then
  SIGHUP k3s. `install_gvisor` in `cli/src/commands/setup.rs` (run by
  `sycoadm setup`) does exactly this. Production clusters use a
  node-installer DaemonSet or a pre-baked AMI.

Why Cilium/Kyverno aren't bundled: a Helm meta-chart can't install subcharts
into different namespaces — `dependencies[].namespace:` on Chart.yaml is
non-functional in Helm 4 (helm/helm#10905). Cilium must live in `kube-system`;
Kyverno must live in `kyverno`. `sycoadm setup` sequences separate helm
installs to get each in its right place.

## Install — hobbyist

```sh
# One command. Creates the k3d cluster with k3s's bundled flannel and
# network-policy controller disabled (Cilium must be the only CNI), then
# installs gVisor, Cilium → kube-system and Kyverno → kyverno (with PSA
# labels).
cargo run -p sycoadm --release -- setup
```

`sycoadm setup` lives in the workspace's `cli/` crate. It uses `helm upgrade
--install` everywhere, so re-running is idempotent. It installs the
`sycophant-gvisor`, `kyverno-crds` and `sycophant-cluster` charts directly,
not through this bundle.

## Install — DevOps

Cilium and Kyverno are your responsibility. Once they're in place per the
prerequisites above:

```sh
cd charts/sycophant-quickstart
helm dependency update
kubectl apply -f ../sycophant-cluster/system-ns.yaml
helm install sycophant . -n sycophant-system --wait --timeout=10m
```

Then provision tenants by writing command ConfigMaps. A command is admitted
only when it is created under an IdP user login (`<issuer>#<sub>`), never a
service account, so whoever writes commands needs a login from your IdP.

Let that identity write commands in `sycophant-system` (apply this with your
admin credential):

```yaml
apiVersion: rbac.authorization.k8s.io/v1
kind: Role
metadata: {name: marker-writer, namespace: sycophant-system}
rules:
  - {apiGroups: [""], resources: [configmaps], verbs: [create, delete]}
---
apiVersion: rbac.authorization.k8s.io/v1
kind: RoleBinding
metadata: {name: marker-writer, namespace: sycophant-system}
roleRef: {apiGroup: rbac.authorization.k8s.io, kind: Role, name: marker-writer}
subjects:
  - {apiGroup: rbac.authorization.k8s.io, kind: User, name: "<issuer>#<sub>"}
```

Create the tenant under that login. `owner` is that same login, which then
owns the tenant; `owner-hash` is the first 16 hex characters of its SHA-256
(`printf %s '<issuer>#<sub>' | shasum -a 256 | cut -c1-16`). The command is
named by the tenant hash, the first 10 hex characters of the SHA-256 of the
owner, a newline, and the chosen name
(`printf '%s\n%s' '<issuer>#<sub>' acme | shasum -a 256 | cut -c1-10`), which
is also `tenant`; the chosen name rides in `sycophant.md/target`.
Delete and re-create the command to upgrade the tenant in place:

```yaml
apiVersion: v1
kind: ConfigMap
metadata:
  name: provision-tenant-<10 hex>
  namespace: sycophant-system
  annotations:
    sycophant.md/target: acme
  labels:
    sycophant.md/command: "true"
    app.kubernetes.io/part-of: sycophant
data:
  tenant: "<10 hex>"
  capacity: 12Gi
  owner: "<issuer>#<sub>"
  owner-hash: "<16 hex>"
```

When the tenant is up, apply the same Role and RoleBinding in `tenant-<10 hex>`
so the owner can write that tenant's commands (agents, channels,
toolsets).

## Uninstall

This chart's uninstall is straightforward — no Cilium/Kyverno entanglement:

```sh
helm uninstall sycophant -n sycophant-system --timeout=5m
```

For a `sycoadm setup` cluster, delete the whole cluster instead (every
tenant and all data; irreversible):

```sh
cargo run -p sycoadm --release -- destroy
```

## Why a sibling kyverno-crds chart

Bundling Kyverno's CRDs into Kyverno's own chart would mean `helm uninstall
kyverno` cascade-deletes every Kyverno policy and `PolicyException` in the
cluster. The sibling `kyverno-crds` chart pins the CRD lifecycle to a separate
helm release so policies survive teardown.

Cilium's CRDs are installed by the cilium-operator at runtime (not via helm),
so they need no sibling chart — they're already outside helm's ownership.

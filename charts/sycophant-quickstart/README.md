# sycophant-quickstart

Cluster-scoped sycophant install. Ships the gVisor RuntimeClass and the
cluster-wide RBAC, Kyverno policies and admission policies that sycophant
tenant namespaces depend on.

## What's inside

| Subchart | Purpose |
|---|---|
| `sycophant-gvisor` | gVisor RuntimeClass definition |
| `sycophant-cluster` | cluster-scoped RBAC + Kyverno policies + ValidatingAdmissionPolicies |

Per-tenant charts are not bundled. Each tenant's components are provisioned separately.

## Prerequisites

Cilium, Kyverno and the `kyverno-crds` chart are not bundled. `sycoadm setup`
installs all three. On your own cluster, you install:

- **Cilium 1.19.7 or later in `kube-system`** with the L7 DNS proxy enabled,
  kube-proxy replacement disabled, and a pod CIDR matching the cluster's pod
  range. Toolset egress allowlists depend on CiliumNetworkPolicy, so Calico or
  Flannel cannot replace it.
- **The `kyverno-crds` chart as its own Helm release, before Kyverno.** Kyverno
  does not come Ready without its CRDs. The chart lives at `charts/kyverno-crds/`.
- **Kyverno 3.9.x in the `kyverno` namespace**, with `config.excludeKyvernoNamespace: true`
  (the chart default), `crds.install: false` (the kyverno-crds chart owns
  them), the reports controller disabled, and the
  `kyverno` namespace carrying `pod-security.kubernetes.io/enforce: restricted`.
- **`runsc` on each node** provides the gVisor RuntimeClass that isolates
  capability-job pods. On k3d, `install_gvisor` in `cli/src/commands/setup.rs`
  installs it. Production clusters use a node-installer DaemonSet or a
  pre-baked AMI.

Cilium and Kyverno aren't bundled because a Helm chart can't install subcharts
into different namespaces (helm/helm#10905). Cilium must live in `kube-system`
and Kyverno in `kyverno`. `sycoadm setup` installs each with its own Helm
release.

## Install: hobbyist

```sh
# One command. Creates the k3d cluster with k3s's bundled flannel and
# network-policy controller disabled (Cilium must be the only CNI), then
# installs gVisor, Cilium → kube-system and Kyverno → kyverno (with Pod
# Security labels).
cargo run -p sycoadm --release -- setup
```

`sycoadm setup` lives in the `cli/` crate. It uses `helm upgrade --install`
everywhere, so it is safe to repeat. It installs the
`sycophant-gvisor`, `kyverno-crds` and `sycophant-cluster` charts directly,
not through this bundle.

## Install: DevOps

Once the prerequisites are in place:

```sh
cd charts/sycophant-quickstart
helm dependency update
kubectl apply -f ../sycophant-cluster/system-ns.yaml
helm install sycophant . -n sycophant-system --wait --timeout=10m
```

Then provision tenants by writing command ConfigMaps. Only a user login
(`<issuer>#<sub>`) can create a command, never a service account.

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

Create the tenant under that login. `owner` is that login, which then owns the
tenant. `owner-hash` is the first 16 hex characters of its SHA-256
(`printf %s '<issuer>#<sub>' | shasum -a 256 | cut -c1-16`). The tenant hash
names the command and is also `tenant`. It is the first 10 hex characters of
the SHA-256 of the owner, a newline and the chosen name
(`printf '%s\n%s' '<issuer>#<sub>' acme | shasum -a 256 | cut -c1-10`). The
chosen name goes in `sycophant.md/target`. To upgrade the tenant in place,
delete and re-create the command:

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

```sh
helm uninstall sycophant -n sycophant-system --timeout=5m
```

For a `sycoadm setup` cluster, delete the whole cluster instead (every
tenant and all data, with no undo):

```sh
cargo run -p sycoadm --release -- destroy
```

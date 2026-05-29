# cluster-resources/

Chart-shipped cluster-scoped resource shape checks. Each test asserts a
single cluster-scoped object (RuntimeClass, ClusterPolicy, etc.) exists
with its expected fields. Separate from `workspace-pod-shape/` (which is
Pod admission) and `tenant-resource-protection/` (which is per-tenant).

| Test                              | What it proves                                                    |
|-----------------------------------|-------------------------------------------------------------------|
| runtimeclass-gvisor-handler/      | `RuntimeClass/gvisor` declares `handler: runsc` (real gVisor)     |

Belongs here: chainsaw assertions on cluster-scoped resources shipped by
the chart (sycophant-cluster, sycophant-gvisor, kyverno-crds). Doesn't
belong here: pod admission (`workspace-pod-shape/`), tenant lifecycle
(`tenant-resource-protection/`, `tenant-namespace-creation/`), or SA
impersonation (`sa-permission-bounds/`).

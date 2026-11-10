# cluster-resources/

Chart-shipped resource shape and naming checks. Most assert a single
cluster-scoped object (RuntimeClass, Kyverno policy, etc.) exists with its
expected fields. Some pin the render-time name shape of chart-shipped
resources. Separate from `harness-pod-shape/` (which is Pod admission)
and `tenant-resource-protection/` (which is per-tenant).

| Test                                    | What it proves                                                          |
|-----------------------------------------|------------------------------------------------------------------------|
| runtimeclass-gvisor-handler/            | `RuntimeClass/gvisor` declares `handler: runsc` (real gVisor)          |
| cluster-chart-requires-kyverno-crds/    | Rendering without the Kyverno CRDs hard-fails naming the missing CRD    |
| provision-tenant-command-materializes-installer/ | LIVE: an admitted tenant provision command materializes the uid-named ServiceAccount, Job, Role and RoleBinding. The Job succeeds and deletes the command |

Belongs here: chainsaw assertions on the shape or render-time naming of
resources shipped by the charts (sycophant-cluster, sycophant-gvisor,
kyverno-crds, sycophant-tenant). Doesn't
belong here: pod admission (`harness-pod-shape/`), tenant lifecycle
(`tenant-resource-protection/`, `tenant-namespace-creation/`), or ServiceAccount
impersonation (`sa-permission-bounds/`).

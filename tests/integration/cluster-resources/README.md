# cluster-resources/

Chart-shipped cluster-scoped resource shape checks. Each test asserts a
single cluster-scoped object (RuntimeClass, ClusterPolicy, etc.) exists
with its expected fields. Separate from `transponder-pod-shape/` (which is
Pod admission) and `tenant-resource-protection/` (which is per-tenant).

| Test                                    | What it proves                                                          |
|-----------------------------------------|------------------------------------------------------------------------|
| runtimeclass-gvisor-handler/            | `RuntimeClass/gvisor` declares `handler: runsc` (real gVisor)          |
| policy-engine-external-skips/           | `policyEngine=external` renders none of the three ClusterPolicies, no error |
| policy-engine-kyverno-missing-crds-fails/ | `policyEngine=kyverno` without Kyverno CRDs hard-fails naming the CRD  |
| policy-engine-unset-fails/              | Unset `policyEngine` fails the install (schema `required`)             |
| policy-engine-invalid-rejected/         | Out-of-enum `policyEngine` rejected by schema validation              |

Belongs here: chainsaw assertions on cluster-scoped resources shipped by
the chart (sycophant-cluster, sycophant-gvisor, kyverno-crds). Doesn't
belong here: pod admission (`transponder-pod-shape/`), tenant lifecycle
(`tenant-resource-protection/`, `tenant-namespace-creation/`), or SA
impersonation (`sa-permission-bounds/`).

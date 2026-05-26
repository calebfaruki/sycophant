# privileged-workload-rejection/

PSA must reject privileged pods in sycophant-labeled namespaces. This bucket
tests PSA enforcement in places that ARE NOT workspace pods, where the VAP
doesn't fire. The orthogonality test in `tenant-namespace-non-workspace/`
also asserts that the workspace VAP stays silent — proves PSA and VAP are
doing different jobs.

| Test                              | Loads on                                                                  |
|-----------------------------------|---------------------------------------------------------------------------|
| infra-namespace/                  | `charts/sycophant-quickstart/templates/infra-ns.yaml` carries PSA labels  |
| tenant-namespace-non-workspace/   | Sycophant mutate rule stamps PSA on deployer-created ns                   |

Belongs here: PSA-driven rejections in non-workspace contexts.
Doesn't belong here: workspace-pod admissions (see `workspace-pod-shape/`).

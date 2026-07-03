# privileged-workload-rejection/

PSA must reject privileged pods in sycophant-labeled namespaces. This bucket
tests PSA enforcement in places that ARE NOT transponder pods, where the VAP
doesn't fire. The orthogonality test in `tenant-namespace-non-workspace/`
also asserts that the transponder VAP stays silent — proves PSA and VAP are
doing different jobs.

| Test                              | Loads on                                                                  |
|-----------------------------------|---------------------------------------------------------------------------|
| system-namespace/                 | `syco setup` creates `sycophant-system` PSA-restricted (SYSTEM_NS_YAML)    |
| tenant-namespace-non-workspace/   | Perimeter-labeled deployer-created ns enforces PSA on non-workspace pods   |

Belongs here: PSA-driven rejections in non-workspace contexts.
Doesn't belong here: transponder-pod admissions (see `transponder-pod-shape/`).

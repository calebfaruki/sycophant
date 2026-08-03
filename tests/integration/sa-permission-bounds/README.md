# sa-permission-bounds/

Tests that pin the upper bound of each ServiceAccount's authority. Each
test impersonates an SA and asserts a specific action is denied —
whether the wall is RBAC (`kubectl auth can-i` or a real `create`
returning Forbidden) or admission (a ValidatingAdmissionPolicy or
Kyverno rule rejecting the request).

| Test                                  | What it proves                                                                |
|---------------------------------------|-------------------------------------------------------------------------------|
| tenant-deployer-no-get-secrets/       | Deployer can `create` Secrets but not `get` them                              |
| tenant-deployer-no-pods-log/          | Deployer cannot read pod logs cluster-wide                                    |
| tenant-deployer-no-cluster-writes/    | Deployer has no writes outside tenant-* namespaces                            |
| relay-ctrl-no-secret-updates/     | relay can `create+get` Secrets, not `update` (signing key write-once)     |
| relay-secret-name-allowlist/      | relay may only create Secrets named relay-signing-key or bridge-state |
| workspace-sa-no-verbs/                | workspace SA has zero K8s API verbs                                           |

Belongs here: SA-impersonation probes (`kubectl --as` or `kubectl auth
can-i`) asserting denied actions, regardless of which layer enforces.
Doesn't belong here: tests scoped to a particular workload shape (see
`harness-pod-shape/`) or a specific tenant-resource lifecycle (see
`tenant-resource-protection/`).

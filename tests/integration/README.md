# integration tests

Live-cluster admission and RBAC tests using chainsaw. Each directory is a
named security property. Tests inside route through real sycophant code via
the tenant-deployer SA — no fixture short-circuits.

## Buckets

| Bucket                          | Property under test                                                  |
|---------------------------------|----------------------------------------------------------------------|
| privileged-workload-rejection/  | PSA rejects privileged pods in sycophant namespaces (orthogonality)  |
| transponder-pod-shape/            | Transponder VAP enforces every required field on transponder pods    |
| tenant-namespace-creation/      | Only deployer can create tenant ns; perimeter label required         |
| tenant-resource-protection/     | Same-ns SAs cannot tamper with their own NetworkPolicy/RBAC/etc.     |
| job-controller-allowlist/       | Only chart-installed controller SAs may create LLM/airlock Jobs      |
| sa-permission-bounds/           | tenant-deployer + controller SAs hold only the verbs/names claimed   |
| sa-token-audience/              | Apiserver enforces SA-token audience: one token, one controller      |
| cluster-resources/              | Chart-shipped cluster-scoped resources (RuntimeClass, etc.) shape    |

## Picking a bucket for a new test

Ask: "What property is this test asserting?"

- Pod admission shape under VAP → `transponder-pod-shape/`
- Namespace lifecycle (create / label / name) → `tenant-namespace-creation/`
- Same-tenant write isolation → `tenant-resource-protection/`
- Job-by-actor → `job-controller-allowlist/`
- Verb-by-actor or name-by-actor (SA impersonation) → `sa-permission-bounds/`
- SA-token audience handling → `sa-token-audience/`
- Chart-shipped cluster-scoped resource shape → `cluster-resources/`
- "PSA does X" — usually wrong bucket; PSA is upstream, not sycophant.

Do not create a `misc/` or `other/` bucket. Force a property decision.

## Running

```bash
chainsaw test tests/integration --config tests/integration/.chainsaw.yaml
chainsaw test tests/integration/tenant-resource-protection --config tests/integration/.chainsaw.yaml
chainsaw test tests/integration/transponder-pod-shape/projected-sa-token-rejected --config tests/integration/.chainsaw.yaml
```

## Authoring conventions

- Tenant namespaces are created via `command: kubectl create namespace X --as=system:serviceaccount:infra:tenant-deployer`. The mutate rule stamps PSA + perimeter labels; the generate rule produces the per-tenant VAPBinding. Both must be asserted explicitly before the test workload submits.
- Pod fixtures live in `fixtures/` and use `($target_namespace)` for templated namespace. Inline pod manifests in the test file are fine for one-offs.
- Cleanup goes in `finally:` on the last step that owns the namespace. Chainsaw 0.2.12 does not support `spec.cleanup`.
- For impersonation across multiple identities in one test, use `script:` blocks with `kubectl --as=...`. The `command:` form gives cleaner diagnostics but is one-arg-set per step.
- Match denial messages on the shortest unique substring of the policy's `message:` field. Long matches break the moment someone reflows the YAML.

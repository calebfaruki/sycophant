# integration tests

Live-cluster admission and RBAC tests using chainsaw. Each directory is a
named security property. Tests inside route through real sycophant code via
the tenant-deployer SA — no fixture short-circuits.

## Buckets

| Bucket                          | Property under test                                                  |
|---------------------------------|----------------------------------------------------------------------|
| privileged-workload-rejection/  | PSA rejects privileged pods in sycophant namespaces (orthogonality)  |
| harness-pod-shape/            | Harness VAP enforces every required field on harness pods    |
| tenant-namespace-creation/      | Only deployer can create tenant ns; perimeter label required         |
| tenant-resource-protection/     | Same-ns SAs cannot tamper with their own NetworkPolicy/RBAC/etc.     |
| job-controller-allowlist/       | Only chart-installed controller SAs may create LLM/airlock Jobs      |
| job-egress-baselines/           | llm-job/airlock egress shape: chart fail-closed floor; CLI union pins private-IP providers by toCIDR |
| sa-permission-bounds/           | tenant-deployer + controller SAs hold only the verbs/names claimed   |
| sa-token-audience/              | Apiserver enforces SA-token audience: one token, one controller      |
| cluster-resources/              | Chart-shipped cluster-scoped resources (RuntimeClass, etc.) shape    |
| gvisor-scope/                   | gVisor runtime scope pinned to chambers-only (airlock-job); other components on runc |
| conversation-log-mount/         | Only the harness may mount the `*-conversation-data` PVC (VAP, label-agnostic)   |

## Picking a bucket for a new test

Ask: "What property is this test asserting?"

- Pod admission shape under VAP → `harness-pod-shape/`
- Namespace lifecycle (create / label / name) → `tenant-namespace-creation/`
- Same-tenant write isolation → `tenant-resource-protection/`
- Job-by-actor → `job-controller-allowlist/`
- llm-job/airlock egress policy shape (chart baseline floor or CLI-authored union) → `job-egress-baselines/`
- Verb-by-actor or name-by-actor (SA impersonation) → `sa-permission-bounds/`
- SA-token audience handling → `sa-token-audience/`
- Chart-shipped cluster-scoped resource shape → `cluster-resources/`
- gVisor runtime scope (chambers only) → `gvisor-scope/`
- "PSA does X" — usually wrong bucket; PSA is upstream, not sycophant.

Do not create a `misc/` or `other/` bucket. Force a property decision.

## Running

```bash
chainsaw test tests/integration --config tests/integration/.chainsaw.yaml
chainsaw test tests/integration/tenant-resource-protection --config tests/integration/.chainsaw.yaml
chainsaw test tests/integration/harness-pod-shape/projected-sa-token-rejected --config tests/integration/.chainsaw.yaml
```

## Authoring conventions

- Tenant namespaces are created via `kubectl create namespace X --as=system:serviceaccount:sycophant-system:tenant-deployer`, then the test applies the perimeter labels itself (`app.kubernetes.io/part-of=sycophant-tenant` + the four `pod-security.kubernetes.io/*` labels) — there is no auto-labelling mutate; labeling is the namespace creator's job (chart `tenant-ns.yaml` in prod). The label is what triggers the generate rule that produces the per-tenant VAPBinding + tokenreview CRBs; wait for that wiring before the test workload submits.
- Pod fixtures live in `fixtures/` and use `($target_namespace)` for templated namespace. Inline pod manifests in the test file are fine for one-offs.
- Cleanup goes in `finally:` on the last step that owns the namespace. Chainsaw 0.2.12 does not support `spec.cleanup`.
- For impersonation across multiple identities in one test, use `script:` blocks with `kubectl --as=...`. The `command:` form gives cleaner diagnostics but is one-arg-set per step.
- Match denial messages on the shortest unique substring of the policy's `message:` field. Long matches break the moment someone reflows the YAML.

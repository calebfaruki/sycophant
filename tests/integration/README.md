# integration tests

Live-cluster admission and RBAC tests using chainsaw. Each directory is a
named security property.

## Buckets

| Bucket                          | Property under test                                                  |
|---------------------------------|----------------------------------------------------------------------|
| harness-pod-shape/            | `cluster-gvisor-pod-policy` enforces every required field on harness pods |
| tenant-namespace-creation/      | Perimeter label triggers per-tenant wiring; any name, any creator    |
| tenant-resource-protection/     | Same-namespace ServiceAccounts cannot tamper with their own security resources |
| sa-permission-bounds/           | Each ServiceAccount holds only the verbs and names claimed           |
| cluster-resources/              | Chart-shipped cluster-scoped resources (RuntimeClass, etc.) shape    |
| gvisor-scope/                   | gVisor runtime scope |
| ingress/                        | Caddy pod admission in sycophant-ingress and its certificate-failure log |
| conversation-log-mount/         | Only the harness may mount the `ag-*-conversation` PVC   |
| user-reach/                     | A bound subject reaches exactly the listed agents; bindings and identity data follow the user command |
| namespace-egress-baseline/      | Namespace-wide egress default-deny floor; uncovered pods denied; a toolset pod reaches only its domains |
| capability-job-gate/            | Jobs other than a dispatch Job created by `ag-<agent>` are forced into the hardened envelope and Secret allowlist, or denied |
| dispatch-job-gate/              | An agent's dispatch Job for its sub-agent is admitted, stamped and owned by SA `ag-<sub>`, and goes when that SA goes |
| instructions-read-contract/     | Store-agnostic read contract (S3 and STS only): an agent pod's own token reads exactly its instructions folder, and nothing is written |

## Picking a bucket for a new test

Pick the bucket whose property the test asserts. Pod Security Admission
rejections are upstream Kubernetes, not sycophant. A tenant chart render check
that needs no cluster goes in `tests/unit/tenant-chart/`.

Do not create a `misc/` or `other/` bucket. Force a property decision.

## Authoring conventions

- Tenant namespaces come from `kubectl create namespace` or the shared `step-templates/tenant.yaml`. The test applies the perimeter labels: `app.kubernetes.io/part-of=sycophant-tenant` and the four `pod-security.kubernetes.io/*` labels. No policy adds these labels. In production the tenant chart's `tenant-ns.yaml` sets them. The label triggers the generate rule that makes the per-tenant admission policy binding and token review ClusterRoleBindings. Wait for that wiring before the test workload starts.
- Pod fixtures live in `fixtures/` and use `($target_namespace)` for templated namespace. Inline pod manifests in the test file are fine for one-offs.
- Tenant namespace setup and teardown go through the `tenant.yaml` StepTemplate. A `create-tenant-ns` step calls it with `use.template` and passes `tenant_ns: ($tenant_ns)` in `with.bindings`. The template deletes the namespace in `spec.cleanup` after the whole test, so do not add a namespace-delete `finally:`. Only tests whose setup does more than the bare namespace, such as an extra NetworkPolicy or egress floor, keep a hand-written `finally:`.
- Gotcha: a StepTemplate must not declare a `spec.bindings` default for a binding the caller passes. The default shadows the caller's `with.bindings`, so the namespace is created and deleted under the default name. `tenant.yaml` declares no default for `tenant_ns` on purpose.
- For impersonation across multiple identities in one test, use `script:` blocks with `kubectl --as=...`. The `command:` form gives cleaner diagnostics but takes one argument set per step.
- Match denial messages on the shortest unique substring of the policy's `message:` field. Long matches break the moment someone reflows the YAML.

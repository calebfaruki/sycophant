# capability-job-gate

An admission gate on Job CREATE, keyed on the creator's authenticated identity
`system:serviceaccount:<ns>:ag-<agent>`, forces the Job's pod template into a
hardened, zero-RBAC sandbox bounded to the agent's operator-approved secrets,
or denies the Job. Two per-agent ServiceAccounts carry the create/run split:
`ag-<agent>` holds only `create` on `batch/jobs`; `ag-<agent>-unprivileged` holds no
RBAC and automounts no token.

The gate keys on the unforgeable creating identity, never on a self-asserted pod
label. Agent A's Job may mount only A's approved Secret names, read from the
`ag-<agent>-toolset-secrets` ConfigMap.

## Denial contract

The gate is a single Kyverno `ClusterPolicy` named **`cluster-capability-job-gate`**
(`background: false`, `validationFailureAction: Enforce`). The deny cases run
offline in `tests/unit/kyverno-policies/capability-job-gate-as-harness/`. Each
deny case is well-formed in every respect except the one field it targets, so with
the targeted rule removed the Job admits and the case goes red — no case passes on
a policy missing the rule it targets.

## Tests

| Test                               | Property                                                       |
|------------------------------------|----------------------------------------------------------------|
| well-formed-job-admits-and-stamps/ | A well-formed harness Job admits and is stamped with the envelope, `component: capability-job`, and `agent` overwritten from identity (live) |
| policy-exception-admits/           | A gate-denied Job admits when a matching Kyverno PolicyException exists (live) |

The render checks for the two ServiceAccounts live in `tests/unit/tenant-chart/`.
Live tests impersonate the creator with
`kubectl --as system:serviceaccount:<ns>:ag-<agent>` and observe admit/deny at
admission; they need a cluster because identity-keyed admission is not visible to a
render.

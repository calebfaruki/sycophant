# capability-job-gate

An admission gate on Job create keys on the creator's identity,
`system:serviceaccount:<ns>:ag-<agent>`. It forces the Job's pod template into a
hardened sandbox with no RBAC, bounded to the agent's approved Secrets, or
denies the Job. Two per-agent ServiceAccounts split creating from executing.
`ag-<agent>` creates Jobs. `ag-<agent>-unprivileged` holds no RBAC and
automounts no token.

It never keys on a self-asserted pod label. Agent A's Job may mount only A's
approved Secret names, read from the `ag-<agent>-toolset-secrets` ConfigMap.

## Denial contract

The gate is the Kyverno `ValidatingPolicy` **`cluster-capability-job-gate`**
(`validationActions: [Deny]`, background evaluation off). The deny cases are
checked offline in `tests/unit/kyverno-policies/capability-job-gate-as-harness/`.
Each deny case is valid except for the one field it targets. With that rule
removed, the Job admits and the case goes red, so no case passes on a policy
missing its rule.

## Tests

| Test                               | Property                                                       |
|------------------------------------|----------------------------------------------------------------|
| well-formed-job-admits-and-stamps/ | A well-formed harness Job admits and is stamped with the envelope, `component: capability-job`, and `agent` overwritten from identity (live) |
| policy-exception-admits/           | A gate-denied Job admits when a matching Kyverno PolicyException exists (live) |

The render checks for the two ServiceAccounts live in `tests/unit/tenant-chart/`.
Live tests impersonate the creator with
`kubectl --as system:serviceaccount:<ns>:ag-<agent>` and observe admission. A
render cannot show identity-keyed admission, so they need a cluster.

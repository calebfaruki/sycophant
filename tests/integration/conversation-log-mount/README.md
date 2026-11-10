# conversation-log-mount/

The per-agent conversation log lives on the harness's
`ag-<agent>-conversation` PVC. Only the harness may mount it. Untrusted tool
code executes in separate toolset pods that must never reach the log.

The `cluster-conversation-log-mount` admission policy enforces this. It is
keyed on the **volume** (any pod mounting a `ag-*-conversation` PVC), not on
sycophant labels, so a pod cannot dodge it by omitting a `component` label. Its
binding ships cluster-wide in the cluster chart, so tests need not wait for a
per-tenant binding. Admission is spec-only, so the referenced PVC need not
exist.

| Test                         | Property under test                                            |
|------------------------------|----------------------------------------------------------------|
| rogue-pod-mount-denied/      | A label-less pod mounting a `ag-*-conversation` PVC is denied |

The harness's own mount is admitted in `harness-pod-shape/compliant-pod-admits`.

Belongs here: the conversation-log mount restriction. Doesn't belong here: the
harness pod-shape rules (see `harness-pod-shape/`).

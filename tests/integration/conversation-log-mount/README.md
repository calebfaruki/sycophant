# conversation-log-mount/

The per-workspace conversation log lives on the transponder's
`<ws>-conversation-data` PVC. Only the transponder may mount it — untrusted tool
code runs in separate airlock chamber pods that must never reach the log.

The `cluster-conversation-log-mount` VAP enforces this. It is keyed on the
**volume** (any pod mounting a `*-conversation-data` PVC), not on sycophant
labels, so a pod cannot dodge it by omitting a `component` label. Its binding is
static + cluster-wide (shipped in the cluster chart), so no per-tenant VAPBinding
wait is needed. Admission is spec-only, so the referenced PVC need not exist.

| Test                         | Property under test                                            |
|------------------------------|----------------------------------------------------------------|
| rogue-pod-mount-denied/      | A label-less pod mounting a `*-conversation-data` PVC is denied |
| transponder-mount-admitted/  | A transponder pod mounting it is admitted (guard doesn't break the legit mount) |

Belongs here: the conversation-log mount restriction. Doesn't belong here: the
transponder pod-shape rules (see `transponder-pod-shape/`).

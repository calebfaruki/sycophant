# capability-job-gate

An admission gate on Job CREATE, keyed on the creator's authenticated identity
`system:serviceaccount:<ns>:harness-<ws>`, forces the Job's pod template into a
hardened, zero-RBAC sandbox bounded to the workspace's operator-approved secrets,
or denies the Job. Two per-workspace ServiceAccounts carry the create/run split:
`harness-<ws>` holds only `create` on `batch/jobs`; `unprivileged-<ws>` holds no
RBAC and automounts no token.

The gate keys on the unforgeable creating identity, never on a self-asserted pod
label. Workspace W's Job may mount only W's approved Secret names, read from the
`capability-grants-<ws>` projection ConfigMap.

## Denial contract (fixed input for the implementer)

The gate is a single Kyverno `ClusterPolicy` named **`cluster-capability-job-gate`**
(`background: false`, `validationFailureAction: Enforce`). Every live deny test
below matches that name on the rejection, the way
`tenant-resource-protection/*-immutable` match `cluster-protect-security`. Each
deny case is well-formed in every respect except the one field it targets, so with
the targeted rule removed the Job admits and the test goes red — no case passes on
a policy missing the rule it targets.

## Tests

| Test                               | Property                                                       |
|------------------------------------|----------------------------------------------------------------|
| unprivileged-sa-unbound-no-automount/ | `unprivileged-<ws>` renders with automount false and no RoleBinding subject (render) |
| harness-sa-only-creates-jobs/      | `harness-<ws>` Role is exactly `create` on `batch/jobs` (render) |
| well-formed-job-admits-and-stamps/ | A well-formed harness Job admits and is stamped with the envelope, `component: capability-job`, and `workspace` overwritten from identity (live) |
| stripped-envelope-denies/          | A harness Job with a non-compliant container securityContext denies (live) |
| off-allowlist-volume-kind-denies/  | A harness Job with a volume kind outside the six-kind safe set denies (live) |
| off-allowlist-secret-volume-denies/    | Off-allowlist name on `volumes[].secret.secretName` denies (live) |
| off-allowlist-secret-projected-denies/ | Off-allowlist name on `volumes[].projected.sources[].secret.name` denies (live) |
| off-allowlist-secret-env-denies/       | Off-allowlist name on `env[].valueFrom.secretKeyRef.name` denies (live) |
| off-allowlist-secret-envfrom-denies/   | Off-allowlist name on `envFrom[].secretRef.name` denies (live) |
| off-allowlist-image-pull-secret-denies/ | Off-allowlist name on `imagePullSecrets[].name` denies (live) |
| wrong-service-account-denies/      | `serviceAccountName` other than `unprivileged-<ws>` denies (live) |
| bad-job-kind-denies/               | `sycophant.md/job-kind` outside `{tool, inference}` denies (live) |
| policy-exception-admits/           | A gate-denied Job admits when a matching Kyverno PolicyException exists (live) |
| cross-workspace-secret-denies/     | Workspace A's identity naming workspace B's approved Secret denies (live) |

Render tests use `helm template` alone. Live tests impersonate the creator with
`kubectl --as system:serviceaccount:<ns>:harness-<ws>` and observe admit/deny at
admission; they need a cluster because identity-keyed admission is not visible to a
render.

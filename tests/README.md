# sycophant test suite

Three scopes, organized by what infrastructure they need:

```
tests/
├── unit/         # No cluster. Sub-second policy logic checks (kyverno test).
├── integration/  # Live cluster. Admission + RBAC + Kyverno chain (chainsaw).
└── e2e/          # Live cluster + data plane. Currently inline in scripts/quickstart-test.sh.
```

## Quick start

Install test tooling (idempotent):

```bash
scripts/install-test-deps.sh   # chainsaw + kyverno CLI
```

Run the full integration suite against a live cluster:

```bash
chainsaw test tests/integration --config tests/integration/.chainsaw.yaml
```

Run a single bucket:

```bash
chainsaw test tests/integration/harness-pod-shape --config tests/integration/.chainsaw.yaml
```

Run offline policy logic checks:

```bash
make test-unit-k8s
```

Run the full e2e (k3d cluster bring-up + chainsaw + smoke):

```bash
scripts/quickstart-test.sh
```

## Tests are real, or they're nothing

Every test under `integration/` must route through actual sycophant code paths:

1. **Tenant namespaces are created via the deployer SA**, not inline with PSA labels hardcoded. The sycophant mutate rule is the only thing that can stamp the labels. The test asserts those labels via `kubectl get` — fixture asymmetry. Delete the mutate rule, the test goes red.

2. **Caller identity matches the policy's selector.** SA-gated rules are exercised via `kubectl --as=system:serviceaccount:ns:sa` inside `command:` or `script:` steps.

3. **Assertions are apiserver effects**, not CR existence. `kubectl get clusterpolicy foo` proves nothing. `expect: check: ($error != null): true` on a rejected apply does.

4. **Deny tests pair with accept tests** under the same caller identity. A rule that denies everything passes a deny-only suite.

5. **Mutate → validate → assert in one test.** Apply unlabeled, let mutate run, let validate run, assert the mutated state.

## Mutation-removal smoke check

`scripts/mutation-removal.sh <name>` deletes a sycophant policy from the live
cluster, runs the chainsaw tests that should depend on it, and confirms they
fail. If they still pass, the tests are performative.

Mutations:

| Name              | Removes                                            | Expected to break                                              |
|-------------------|----------------------------------------------------|----------------------------------------------------------------|
| harness-vap   | VAP `cluster-gvisor-pod-policy`             | All `harness-pod-shape/` tests                               |
| protect-security  | ClusterPolicy `cluster-protect-security`         | All `tenant-resource-protection/` |
| tenant-tokenreview-crbs | ClusterPolicy `tenant-rolebinding-generator` | `tenant-namespace-creation/tenant-tokenreview-crbs-generated` |

The script restores the chart via `helm upgrade --install` after.

## Offline layer (kyverno test)

`tests/unit/kyverno-policies/<policy-name>/` directories hold offline policy
checks. Each has:

- `policy.yaml` — rendered from the chart by `make test-unit-k8s`, never hand-edited
- `resource-*.yaml` — synthetic admission inputs
- `user-*.yaml` — UserInfo fixtures for impersonation
- `values.yaml` — Namespace labels the policy's `namespaceSelector` matches on
- `kyverno-test.yaml` — drives the test

One userinfo per test directory (kyverno CLI limitation). For multi-actor
coverage on a single policy, create sibling directories
`<policy>-as-<actor>/` each with their own kyverno-test.yaml. Offline tests
duplicate a subset of chainsaw integration coverage at sub-second speed for
PR feedback.

Two traps this layer sets for the unwary:

1. **A policy with a `namespaceSelector` matches nothing offline** unless the
   Namespace labels are declared in `values.yaml`. Without them every row
   reports `Excluded` and the suite passes without evaluating a single rule.
2. **A deleted rule is not caught.** The CLI emits no result row for a rule that
   is absent and silently drops the expectations naming it, so the suite stays
   green. Offline covers rule *logic*; rule *existence* needs the chainsaw
   apiserver-effect test.

Check the `REASON` column, not just the summary: `Ok` means the rule ran.

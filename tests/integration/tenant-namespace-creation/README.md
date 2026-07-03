# tenant-namespace-creation/

Per-tenant provisioning: when a namespace carries the
`app.kubernetes.io/part-of=sycophant-tenant` label — with any name, created by
any actor — the `tenant-rolebinding-generator` Kyverno policy mints the
cluster-scoped wiring the creating credential is not permitted to make itself.

| Test                               | Loads on                                                                                                  |
|------------------------------------|-----------------------------------------------------------------------------------------------------------|
| tenant-tokenreview-crbs-generated/ | Label-matched generate rules in `tenant-rolebinding-generator` (3 tokenreview CRBs + the pod VAP binding)  |
| tenant-ns-labels-rendered/         | Chart `templates/tenant-ns.yaml` renders the namespace + perimeter/PSA labels (gated on `namespace.create`) |

Belongs here: anything about per-tenant wiring keyed on the perimeter label.
Doesn't belong here: writes INTO an already-created tenant ns
(see `tenant-resource-protection/`).

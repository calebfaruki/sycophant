# tenant-namespace-creation/

Per-tenant provisioning: when a namespace carries the
`app.kubernetes.io/part-of=sycophant-tenant` label, the
`tenant-rolebinding-generator` Kyverno policy creates the cluster-scoped wiring
the creating credential cannot make itself. Any name and any creator work.

| Test                               | Loads on                                                                                                  |
|------------------------------------|-----------------------------------------------------------------------------------------------------------|
| tenant-tokenreview-crbs-generated/ | Label-matched generate in `tenant-rolebinding-generator` (relay tokenreview ClusterRoleBinding + pod admission policy binding) |
| tenant-ns-requires-psa/            | `cluster-tenant-namespace-psa-required` rejects a tenant-labeled namespace missing `enforce=restricted`         |

The namespace render (`templates/tenant-ns.yaml`) is checked in `tests/unit/tenant-chart/`.

Belongs here: anything about per-tenant wiring keyed on the perimeter label.
Doesn't belong here: writes INTO an already-created tenant namespace
(see `tenant-resource-protection/`).

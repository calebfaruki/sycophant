# capability-grants-projection

Chart-render shape of the per-workspace capability-grants projection ConfigMap.

The chart renders one `capability-grants-<ws>` ConfigMap per workspace that
declares credential grants. Its `data` keys are the approved Secret names and
each value names the owning toolset. An admission gate
(`harness-created-credentialed-jobs`) reads these keys as its per-workspace
secret-name allowlist, so the projection must stay a flat name-to-toolset map
with no secret material and no YAML-in-string values.

Every test here renders with `helm template` alone and asserts on the rendered
objects. No cluster admission or controller is involved: template rendering from
the operator's own `.Values.workspaces` grant authoring is what guarantees the
admission view cannot drift from the grants.

| Test                            | Property                                                        |
|---------------------------------|-----------------------------------------------------------------|
| keys-are-approved-secret-names/ | `data` keys are the grants' Secret names; no path/egress leak; type label present |
| values-name-owning-toolset/     | Each key's value names the toolset that owns that grant         |
| one-projection-per-workspace/   | One ConfigMap per workspace, with disjoint keys                 |
| no-projection-without-grants/   | Only grant-bearing workspaces project; grantless ones do not    |
| shared-secret-key-collapses/    | Two grants sharing one Secret render that name as a single key  |

The tamper-protection property (an in-namespace SA cannot write the projection)
lives in `tenant-resource-protection/capability-grants-configmap-immutable`,
because rejection at admission is only observable in-cluster.

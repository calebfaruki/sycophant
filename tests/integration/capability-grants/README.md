# capability-grants

Chart-render shape of the per-workspace capability-grants ConfigMap.

The chart renders one `capability-grants-<ws>` ConfigMap per workspace, empty
when that workspace declares no credential grants. Its `data` keys are the
approved Secret names and each value names the owning toolset. The
capability-job admission gate reads these keys as its per-workspace secret-name
allowlist, so the ConfigMap must stay a flat name-to-toolset map with no secret
material and no YAML-in-string values.

Every workspace gets one because Kyverno fails closed on a missing context
source: an absent ConfigMap denies every Job that workspace's harness creates,
rather than reading as "no approvals".

Every test here renders with `helm template` alone and asserts on the rendered
objects. No cluster admission or controller is involved: template rendering from
the operator's own `.Values.workspaces` grant authoring is what guarantees the
admission view cannot drift from the grants.

| Test                            | Property                                                        |
|---------------------------------|-----------------------------------------------------------------|
| keys-are-approved-secret-names/ | `data` keys are the grants' Secret names; no path/egress leak; type label present |
| values-name-owning-toolset/     | Each key's value names the toolset that owns that grant         |
| renders-only-own-secrets/       | One ConfigMap per workspace, with disjoint keys                 |
| renders-for-every-workspace/    | Every workspace renders one; a grantless one renders empty `data` |
| shared-secret-key-collapses/    | Two grants sharing one Secret render that name as a single key  |

The tamper-protection property (an in-namespace SA cannot write the ConfigMap)
lives in `tenant-resource-protection/capability-grants-configmap-immutable`,
because rejection at admission is only observable in-cluster.

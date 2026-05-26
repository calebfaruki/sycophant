# tenant-namespace-creation/

Namespace lifecycle properties: who can create tenant namespaces, what they
must be named, what labels they must carry.

| Test                                       | Loads on                                                                  |
|--------------------------------------------|---------------------------------------------------------------------------|
| tenant-deployer-stamps-labels/             | Mutate rule `label-tenant-ns` in `tenant-rolebinding-generator`           |
| tenant-deployer-bad-name-rejected/         | Validate rule `enforce-tenant-uuid-name` in `tenant-namespace-naming`     |
| cluster-admin-unlabeled-rejected/          | Validate rule `require-perimeter-label` (NOT actor-gated)                 |
| cluster-admin-labeled-admitted/            | Pair test — proves perimeter rule is not over-broad                       |

Belongs here: anything about namespace CREATE admission for tenant-* names.
Doesn't belong here: writes INTO an already-created tenant ns
(see `tenant-resource-protection/`).

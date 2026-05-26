# rbac-blast-radius/

Verb-by-actor RBAC assertions via `kubectl auth can-i`. No resources are
created; tests only probe the cluster's RBAC state.

| Test                                 | What it proves                                                            |
|--------------------------------------|---------------------------------------------------------------------------|
| tenant-deployer-no-get-secrets/      | Deployer can `create` Secrets but not `get` them                          |
| tenant-deployer-no-pods-log/         | Deployer cannot read pod logs cluster-wide                                |
| tenant-deployer-no-cluster-writes/   | Deployer has no writes outside tenant-* namespaces                        |
| tightbeam-ctrl-no-secret-updates/    | tightbeam can `create+get` Secrets, not `update` (signing key write-once) |
| mainframe-ctrl-no-secret-access/     | mainframe Role has NO Secret verbs at all                                 |

Belongs here: kubectl auth can-i probes against existing or test-installed RBAC.
Doesn't belong here: admission-time rejections (see other buckets).

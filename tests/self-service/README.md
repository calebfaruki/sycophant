# self-service/

Live checks for the cluster with the self-service setting on. The cluster must
be upgraded with `scripts/fixtures/cluster-values.yaml` and `selfService.operator`
set first; the tests act as whatever identity that value names.
`scripts/quickstart-test.sh` does this after the default suite.

| Test                        | What it proves                                                                                          |
|-----------------------------|---------------------------------------------------------------------------------------------------------|
| tenant-lifecycle/           | LIVE: a tenant admin with a record provisions a tenant; installers delete only their own command; the owner gets ConfigMap and Secret create and delete, no Secret read, and reads only the default workspace command; owner and operator re-runs are admitted |
| operator-tenant-command/    | LIVE: the operator provisions a fresh tenant with no record                                             |
| tenant-command-checks/      | Tenant admins create only labelled tenant commands and never update or delete one; no record, a record naming someone else, a non-owner of an existing tenant, and a suspended tenant are refused |
| entitlement-writer/         | Only the entitlement writer creates records and it can do nothing else; a record with no tenant survives the reap |
| owner-grant-before-install/ | The owner grant arrives only once the tenant install finishes                                           |
| service-account-token/      | A tenant namespace refuses service-account-token Secrets, even from cluster-admin                       |
| suspension/                 | LIVE: a suspended tenant refuses in-tenant commands; a second suspend keeps reap-after; only the operator lifts it |
| dex-password-writer/        | The Dex account writer creates Passwords in Dex's namespace and nothing else                            |
| reap-instruction-files/     | LIVE: the reap deletes a tenant's instruction folders, then its record and namespace; a store outage holds the reap |
| reaper-delete-scope/        | The reaper cannot delete a namespace that is not a suspended tenant namespace                           |
| reaper-egress/              | LIVE: the reaper reaches the filer and nothing else in the cluster                                      |

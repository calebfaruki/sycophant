# self-service/

Live checks for the cluster with the self-service setting on. The cluster must
be upgraded with `scripts/fixtures/cluster-values.yaml` and `selfService.operator`
set first. The tests act as whatever identity that value names.
`make test-integration-k8s` does this after the default suite.

| Test                        | What it proves                                                                                          |
|-----------------------------|---------------------------------------------------------------------------------------------------------|
| tenant-lifecycle/           | LIVE: A tenant admin with a record provisions a tenant. Installers delete only their own command. The owner gets ConfigMap and Secret create and delete, no Secret read, and reads only the default agent command. Repeat owner and operator provisions are admitted. |
| operator-tenant-command/    | LIVE: the operator provisions a fresh tenant with no record                                             |
| tenant-command-checks/      | Tenant admins create only labelled tenant commands and never update or delete one. No record, a record naming someone else, a non-owner of an existing tenant, and a suspended tenant are refused |
| entitlement-writer/         | Only the entitlement writer creates records and it can do nothing else. A record with no tenant survives the reap |
| owner-grant-before-install/ | The owner grant arrives only once the tenant install finishes                                           |
| agent-parent/               | An agent command whose parent is a sub-agent, or that gives a parent to an agent with sub-agents, is refused. A sub-agent of a plain agent is admitted |
| service-account-token/      | A tenant namespace refuses service-account-token Secrets, even from cluster-admin                       |
| suspension/                 | LIVE: a suspended tenant refuses in-tenant commands. A second suspend keeps reap-after. Only the operator lifts it |
| dex-password-writer/        | The Dex account writer creates Passwords in Dex's namespace and nothing else                            |
| reap-instruction-files/     | LIVE: the reap deletes a tenant's instruction folders, then its record and namespace. A store outage holds the reap |
| reaper-delete-scope/        | The reaper cannot delete a namespace that is not a suspended tenant namespace                           |
| reaper-egress/              | LIVE: the reaper reaches the filer and nothing else in the cluster                                      |

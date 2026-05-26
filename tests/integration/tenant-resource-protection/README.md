# tenant-resource-protection/

Once a tenant namespace exists, same-namespace ServiceAccounts cannot tamper
with the resources that define their security boundary. The
`cluster-protect-security` ClusterPolicy holds seven rules each protecting
one resource kind. External callers (cluster-admin, mainframe-ctrl) are
excluded via subject or precondition.

One test per protected resource, plus:
- `external-sa-can-write/` — pair test proving the rule isn't a blanket deny
- `protect-self-policy-immutable/` — protects the ClusterPolicy itself (C1.10)

Belongs here: same-ns write/update/delete denials.
Doesn't belong here: rules gated on caller identity that's NOT same-ns
(see `job-controller-allowlist/` for actor-by-name allowlists).

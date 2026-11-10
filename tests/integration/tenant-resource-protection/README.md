# tenant-resource-protection/

Once a tenant namespace exists, same-namespace ServiceAccounts cannot tamper
with the resources that define their security boundary. The
`cluster-protect-security` ValidatingPolicy holds one validation per protected
resource kind. Its match condition
fires only for a caller inside the namespace it writes to, so cluster-admin and
operator ServiceAccounts are not affected.

The per-resource denials are checked offline in `tests/unit/kyverno-policies/`.
The cluster-wide gVisor rule cannot be guarded at admission. Instead, the
provision-write check in `tests/unit/provision-installer/` pins that the
installer's role reaches no admission rule. Live here:
- `network-policy-immutable/`: a ServiceAccount in the namespace cannot create,
  update or delete a NetworkPolicy
- `external-sa-can-write/`: pair test proving the check isn't a blanket deny
- `policy-write-verbs-are-admin-only/`: protects the policies themselves.
  Admission cannot guard them, so the assertion is on RBAC.

Belongs here: same-namespace write, update and delete denials.
Doesn't belong here: rules gated on caller identity from outside the namespace
(see `capability-job-gate/` for the identity-keyed Job CREATE gate).

# user-reach/

A person reaches an agent only through the user command: the installer
renders one reach RoleBinding per listed agent and the relay-identity
ConfigMap that binds the person's channel handles. These tests drive real user
commands through admission and the installer and assert what the bound subject
can and cannot reach.

| Test                                                            | What it proves                                                                 |
|-----------------------------------------------------------------|--------------------------------------------------------------------------------|
| owner-admission-check/                                          | Only the identity recorded as the namespace's owner may write tenant-namespace commands. A non-owner with ConfigMap create RBAC is denied |
| user-command-renders-reach-rolebindings-and-identity-configmap/ | An operator-written user command binds exactly the listed agents and handles. Rewriting it without one cuts it. A writer without namespace write RBAC is denied |

Belongs here: subject reach, handle binding, and the user command's admission
contract. Doesn't belong here: tampering with the identity ConfigMap after it
is rendered (see `tenant-resource-protection/`).

# relay-grants

The grants ConfigMap is the relay's routing and authorization table. The
operator is its only writer, and Helm creates the object without ever owning its
`data`.

Two ownership rules meet here and they pull in opposite directions:

- **Revocation cannot wait for `helm upgrade`.** Rows are runtime data; deleting
  one has to cut access within seconds.
- **An upgrade must not stomp live rows.** The chart has to create the object on
  install, then never touch it again.

The chart resolves that with a template guarded by `.Release.IsInstall`
carrying `helm.sh/resource-policy: keep`. Rendered only at install; absent from
the upgrade manifest, so Helm has nothing to diff; `keep` blocks the delete that
absence would otherwise trigger.

| Test | Property |
|---|---|
| `grants-cm-created-on-install-only/` | Install renders it with `keep` and `sycophant.md/type: relay-grants`; `--is-upgrade` renders it not at all |

Write protection on the object lives in
`tenant-resource-protection/grants-configmap-immutable/` (Kyverno, second lock)
and `sa-permission-bounds/relay-ctrl-grants-and-key-rbac/` (RBAC, first lock).

## Accepted consequences, recorded here so they are not rediscovered

The ConfigMap and its rows survive `helm uninstall`. Uninstall then reinstall
under the same release name re-renders it.

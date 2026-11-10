# namespace-egress-baseline

The namespace-wide egress default-deny floor: one namespace-scoped
CiliumNetworkPolicy that selects every endpoint and grants no egress, so a pod
that no per-component allow policy selects is denied rather than open. The floor
only adds. It removes no existing policy and grants no shared allow.

- `floor-denies-live-egress`: live enforcement, not shape. It applies the floor
  alone and asserts Cilium accepts it (Valid=True). A probe pod with the
  egress-floor-probe label, which no allow policy selects, then tries egress to
  an external IP and must be denied (the pod exits 0). This departs from the
  suite's shape-only rule. The shipped `egress: []` floor passed every shape
  check while enforcing nothing (Valid=False). A floor with
  `enableDefaultDeny.egress: false` is Valid=True yet denies nothing, so only
  the live probe catches it.
- `toolset-reaches-only-its-domains`: live. It applies the agent chart's own
  toolset egress policy for one toolset with github.com, then a probe pod with
  that toolset's labels must reach github.com and must not resolve or reach
  any other host.

Render-only checks (the floor's shape, the install-wait egress allow,
headscale egress hosts) live in `tests/unit/tenant-chart/`.

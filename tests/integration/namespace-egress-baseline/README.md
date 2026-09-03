# namespace-egress-baseline

The namespace-wide egress default-deny floor: one namespace-scoped
CiliumNetworkPolicy that selects every endpoint and grants no egress, so a pod
that no per-component allow policy selects is denied rather than open. The floor
is additive — it strips no existing policy and carries no shared allow.

- `floor-denies-uncovered-egress` — the `default-deny-egress` policy has an empty
  endpointSelector (every endpoint), `enableDefaultDeny.egress: true` (the only
  thing that arms default-deny on Cilium v1.19.3), and exactly one empty egress
  rule (`egress: [{}]`): a rule section is required for the policy to be accepted
  on v1.19.3, and the single rule grants nothing so the floor stays a pure deny;
  kind is the namespaced CiliumNetworkPolicy, never clusterwide.
- `floor-denies-live-egress` — live enforcement, not shape. It applies the floor
  alone, asserts Cilium accepts it (status Valid=True), then a probe pod carrying
  the workspace-init label attempts egress to an external IP and must be denied
  (pod exits 0 / phase Succeeded). This deliberately departs from the suite's
  shape-only convention: the shipped `egress: []` floor passed every shape check
  while being inert (Valid=False, enforcing nothing), so the behavioral claim is
  verified against a real pod. A Valid=True check alone cannot catch a floor with
  `enableDefaultDeny.egress: false`, which is valid yet denies nothing — only the
  live probe does.
- `no-policy-selects-workspace-init` — in the whole-chart render the floor covers
  the workspace-init pod (which reaches no network destination) and no
  CiliumNetworkPolicy or NetworkPolicy adds an egress allow selecting it.
- `baseline-omits-dns-floor-and-preserves-names` — the baseline reuses no shared
  `kube-dns` DNS floor (a pure deny), and every existing per-component egress /
  ingress policy keeps its name.
- `install-wait-reaches-only-apiserver` — under the floor the install-wait allow
  (`install-wait-egress`) grants exactly one egress: `toEntities: [kube-apiserver]`
  on 6443/TCP, with no DNS, FQDN, or CIDR hole.
- `headscale-reaches-only-control-endpoints` — the headscale allow
  (`headscale-egress`) renders only when headscale is enabled, reaches
  `controlplane.tailscale.com` (and `acme-v02.api.letsencrypt.org` only under
  ACME) as both an L7 DNS matchName and a toFQDNs matchName, and carries no
  `matchPattern` wildcard.

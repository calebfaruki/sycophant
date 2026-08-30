# job-egress-baselines

The shape of every Cilium policy that governs tool-job egress: kube-dns stays
an L7 (`rules.dns`) allowlist and real destinations are named `toFQDNs`, never
a CIDR, entity, or wildcard.

- `baseline-dns-allowlist` — the universal fail-closed floor keeps its kube-dns
  rule at L7 and its egress to the toolset controller; a regression to an
  L4-only DNS rule would shadow every sibling allowlist.
- `grant-egress-fqdn-shape` — a per-grant credential policy is scoped to its
  exact (workspace, toolset, grant) triple, reaches only its named domain, and
  a secret-only grant renders no policy.
- `profile-egress-derived-destination` — a prompt profile's egress hole is
  derived from its `baseUrl` and can never be unmatchable: a dotted host yields
  an L7 DNS + `toFQDNs` rule, an IP literal a `toCIDR` with no DNS entry, an
  inference-Service host a `toEndpoints` rule and nothing external; an
  unclassifiable or mismatched host fails the render, and the floor is untouched.

# gvisor-scope

Pins the gVisor runtime scope to **chambers only** (`airlock-job`).

- `chamber-gets-gvisor` — a chamber pod submitted without `runtimeClassName`
  is stamped `gvisor` by the `cluster-runtime-class` mutate and admitted by
  the `cluster-gvisor-pod-policy` VAP. Proves chambers stay sandboxed.
- The companion `harness-pod-shape/compliant-pod-admits` test proves the
  inverse: a harness pod admits on the kubelet-default runtime (no
  `runtimeClassName`), since the mutate no longer stamps it and the VAP no
  longer requires it for non-chamber components.

The harness/llm-job/channel components run on runc; their containment is
the universal VAP envelope (drop-ALL caps, ROFS, automountSA=false, seccomp,
runAsNonRoot) plus per-component Cilium egress allowlists.

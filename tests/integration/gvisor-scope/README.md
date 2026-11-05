# gvisor-scope

Pins the gVisor runtime scope to **toolsets, channels and the ingress**
(`capability-job`, `channel`, `ingress`).

- `capability-job-in-gvisor-scope` — every `capability-job` pod is
  gVisor-sandboxed and volume-bounded no matter which agent's harness
  built the Job, the gVisor rule names `ingress`, and the runtime-class
  mutate names exactly `channel`, `capability-job` and `ingress`. Proves
  toolsets stay sandboxed and pins the scope.
- `channel-in-gvisor-scope` — every `channel` pod is gVisor-sandboxed. Proves
  channels stay sandboxed.
- The companion `harness-pod-shape/compliant-pod-admits` test proves the
  inverse: a harness pod admits on the kubelet-default runtime (no
  `runtimeClassName`).

The harness runs on runc; its containment is
the universal VAP envelope (drop-ALL caps, ROFS, automountSA=false, seccomp,
runAsNonRoot) plus per-component Cilium egress allowlists.

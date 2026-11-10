# gvisor-scope

Pins the gVisor runtime scope to **toolsets, channels and the ingress**
(`capability-job`, `channel`, `ingress`).

- `capability-job-in-gvisor-scope` — every `capability-job` pod is
  gVisor-sandboxed and volume-bounded no matter which agent's harness
  built the Job, the gVisor rule names `ingress`, and the runtime-class
  mutate names exactly `channel`, `capability-job` and `ingress`.
- `channel-in-gvisor-scope` — every `channel` pod is gVisor-sandboxed.

The harness uses runc. Its containment is the shared pod policy (all
capabilities dropped, read-only root, no automounted token, seccomp,
runAsNonRoot) plus per-component Cilium egress allowlists.

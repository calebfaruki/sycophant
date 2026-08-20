# relay-ports

The relay's three listeners and the single ingress CiliumNetworkPolicy that
fences them.

The relay serves the harness port (9090), the app port (9091), and the adapter
port (9092). All three ingress rules live in ONE object, `relay-ingress`. That
is a deliberate choice with a security consequence: any ingress policy selecting
a pod flips that pod to default-deny for ingress, so a missing or mistyped rule
inside the object fails **closed**. Only whole-object absence fails open, and
that same absence drops the harness link, which is loud and immediate.

| Test | Property |
|---|---|
| `adapter-port-ingress-rule/` | Exactly one ingress CNP selects the relay, and it carries the adapter-port rule; the pod and Service publish 9092 |
| `adapter-port-rule-names-adapter-class/` | The adapter rule names `component: adapter` AND `adapter-class: principal`, key equals value; `sycophant.md/channel` appears in no policy selector |

## Where adapter-port reachability is proved

A `principal`-labelled pod reaches 9092 and a pod without that label is refused.
That pair is proved live in `scripts/e2e.sh` as an accept/deny probe pair. It is
not a chainsaw test: live reachability probes
belong in e2e, which is the standing decision the egress-probe tests in this
suite already record.

What lives here instead is the render-time guard.
`adapter-port-rule-names-adapter-class/` covers the fail-open defect the live
probe cannot catch cheaply on every run: an empty or under-specified Cilium
endpoint selector matches every endpoint, so the port stays reachable from the
right pod while the fence quietly admits every other one. That is a shape
defect, it is observable in the render, and it is checked on every chainsaw run
rather than once per e2e.

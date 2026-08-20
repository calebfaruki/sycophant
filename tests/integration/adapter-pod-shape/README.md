# adapter-pod-shape

Channel adapters are Deployments rendered by the chart from `.Values.channels`.
One Deployment per entry, `replicas: 1`, `strategy: Recreate`, full isolation
stack per pod, no workspace mount.

Every claim in this bucket regresses **silently**: a wrong value still renders,
still applies, and still runs. That is what these tests exist for. `helm lint`
and a successful deploy prove nothing about any of them.

| Test | Property |
|---|---|
| `relay-renders-no-tailscale-container/` | No tailscale/tsnet container in the relay pod for any values combination; the tsnet state-Secret Role leaves the relay SA |
| `app-adapter-pod-shape/` | Replicas/strategy, gvisor, automount off, single-audience token, `TS_USERSPACE=true`, no capability adds, no PVCs |
| `app-adapter-is-transport-class/` | `adapter-class: transport`, channel label from the map key, egress policy keyed on the class, no ingress policy selecting the pod |
| `no-channels-renders-no-adapter/` | `channels: {}` renders no adapter workload and no adapter-facing relay ingress rule |
| `unknown-channel-kind-fails-render/` | An unrecognized `kind` fails the render non-zero, naming the kind |
| `relay-ingress-carries-three-rules/` | One object, three rules, and the app port admits only `adapter-class: transport` |

## Why the pod shape is asserted against `helm template`

Chart-rendered security fields are asserted on the chart's own output, never on
hand-written YAML. Each test pipes the render through
`kubectl apply --dry-run=client` so the fields are read off real parsed objects
— a template-trim bug that glues two documents together cannot pass by leaving
the right substrings in the text.

## The one-word failures

`transport` and `principal` are one word apart. A `principal` stamp on the app
adapter admits a transport pipe through the adapter-port fence in
`relay-ports/`, and nothing else in the system would catch the swap.
`TS_USERSPACE` is the other: kernel-TUN mode satisfies neither the gVisor
runtime class nor the capability floor, and the pod simply fails to start with
`tstun.New("tailscale0"): operation not permitted` — at deploy time, in a live
tenant.

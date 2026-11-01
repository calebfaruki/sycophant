# ingress

Live checks on the ingress chart's Caddy pod. Needs the cluster chart and
`sycophant-caddy:local` imported into the k3d node, both from `sycoadm setup`
run from the checkout. Uses the fixed `sycophant-ingress` namespace and
NodePort 30444, so it cannot run beside a real ingress on the same cluster.

- `caddy-pod-policy-and-cert-failure` — the pod policy is bound in
  `sycophant-ingress` and refuses a writable root filesystem with its own
  message; the chart's Caddy pod runs Ready under gVisor and logs
  `could not get certificate from issuer` when its ACME directory is
  unreachable.

# tool-dispatch-direction

The tool-dispatch connection runs harness-to-pod: the harness dials the serving
tool-job pod, and a pod-initiated connection to the harness is denied. These
buckets assert the rendered chart shape that enforces the direction:

- `harness-egress-flips-to-tool-pod` — the per-workspace harness CNP no longer
  admits any ingress on the old dispatch port (9090), and now dials the
  capability-job pods on 9090.
- `tool-pod-admits-only-harness` — a capability-job pod policy admits its
  same-workspace harness on 9090 (the pod-facing ingress that did not exist
  while the pod was a client).
- `headless-service-addresses-pods` — each workspace has a headless
  (`clusterIP: None`) Service selecting its capability-job pods, so the harness
  can reach each pod by its per-pod DNS record.

The live L4 drop is Cilium's to enforce and is out of scope here; these are pure
chart-render assertions that self-apply/read the template into a chainsaw ns.

# inference-workload

Chart-rendered shape of the in-cluster inference workload: one always-on
server per declaring profile, fenced to its tenant.

- `renders-only-when-declared` — default values render nothing; one entry
  renders exactly one single-replica Deployment and one ClusterIP Service;
  unknown entry keys, an out-of-enum `weightsDelivery`, and an entry naming
  an absent profile fail the render.
- `pod-shape` — hardened floor on every container including the init
  container, no SA token, node selector and toleration, and the absence set
  (no runtimeClassName, host namespace, hostPath, or hostPort).
- `engine-caps-from-values` — declared caps reach argv and pod limits, a
  full override moves each one, and no model-fetch or extra-surface flag is
  passed.
- `weight-delivery-paths` — copy is the default (init container copies from
  the weight image into a size-limited emptyDir); `mount` renders an image
  volume and no copy step; neither path invokes a fetch tool.
- `chat-template-shape` — a set `chatTemplate` renders four parts together (a
  ConfigMap holding the content, a volume backed by it, a read-only single-file
  server mount outside the weights path, and the `--chat-template-file` arg);
  the copy and mount delivery modes render that plumbing byte-identical; an
  unset field renders none of it and leaves the server args as today's set;
  setting the field adds no ServiceAccount, token, or Service port; and the
  schema accepts the optional string while still rejecting unknown keys.
  Mutation guard: drop any one part, gate the template on `weightsDelivery`,
  render it unconditionally, add an identity or a second port, flip the mount
  writable, or widen the schema, and exactly one arm reds.
- `netpol-shape` — ingress fenced to the profile's prompt jobs on one port
  with a closed L7 route allowlist, egress DNS-only, port exclusivity across
  the whole chart, and no policy at all without an entry.
- `node-separation` — default renders the inference node selector and
  toleration; `singleNodeException: true` drops both; toolset scheduling
  tolerates only its own class.

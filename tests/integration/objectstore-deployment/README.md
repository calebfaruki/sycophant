# objectstore-deployment/

The shape of the objectstorage Deployment the `charts/sycophant-objectstore`
chart renders: the SeaweedFS pod, its config-driven restart, and its rollout
strategy. Render-only shape checks, no OIDC/IAM trust or network policy.

- `deployment-seaweedfs-iam` — the Deployment runs SeaweedFS with a mounted
  `-s3.iam.config`, carries no `MINIO_*` env, and keeps the hardened pod and
  container securityContext.
- `deployment-config-checksum` — the pod template carries a `checksum/config`
  annotation that changes when the rendered IAM config changes, so a data-only
  config change still rolls the pod.
- `deployment-config-checksum-stable` — `checksum/config` is identical across two
  renders with identical values, so a no-op upgrade does not roll the pod.
- `deployment-s3-readiness-probe` — the seaweedfs container carries a
  readinessProbe on port 8333, so readiness gates on the s3 gateway being up.
- `deployment-strategy-recreate` — the Deployment uses the `Recreate` strategy,
  because the store holds a ReadWriteOnce PVC.

Belongs here: objectstorage Deployment/pod shape and rollout.
Doesn't belong here: OIDC/IAM trust (see `objectstore-iam/`), issuer egress (see
`objectstore-issuer-egress/`), and the store ingress lock (see
`objectstore-ingress-lock/`).

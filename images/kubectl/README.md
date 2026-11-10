# sycophant-kubectl

Minimal Alpine + `kubectl` image. Used by chart-internal Jobs that need to
call the Kubernetes API — currently the post-install wait Job in
`charts/sycophant-tenant/templates/install-wait-job.yaml`.

## Why a custom image

Public kubectl images either lack a shell (rancher/kubectl is distroless) or
carry licensing / pinning friction (Bitnami's free catalog moves through
`bitnamilegacy/` with version-pinned tags that drift). A 6-line Dockerfile
gives us a deterministic, smaller alternative we control.

## Build + load

`sycoadm setup` builds this from a repo checkout and loads it into k3d as
`sycophant-kubectl:local`. The tenant chart's `installWait` values default to
the same image and tag with `pullPolicy: Never`, so kubelet uses the
locally-imported image and never pulls.

## Bumping kubectl version

1. Update the `KUBECTL_VERSION` `ARG` default in `Dockerfile`.
2. If the new version drifts from k3s/k3d's server version, verify kubectl
   skew compatibility (kubectl is officially +/-1 minor; basic operations
   like `create secret` tolerate wider skew).
3. Run `scripts/e2e.sh --clean` (a reuse run keeps the loaded images).

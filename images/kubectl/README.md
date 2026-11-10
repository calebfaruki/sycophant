# sycophant-kubectl

Minimal Alpine + `kubectl` image for chart Jobs that call the Kubernetes API.
Today that is the post-install wait Job in
`charts/sycophant-tenant/templates/install-wait-job.yaml`.

## Why a custom image

Public kubectl images either lack a shell (rancher/kubectl is distroless) or
have tags that drift (Bitnami's free catalog moved to `bitnamilegacy/`). A
short Dockerfile we control avoids both.

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
3. Test with `scripts/e2e.sh --clean`. Without `--clean` it keeps the loaded images.

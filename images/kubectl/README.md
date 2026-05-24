# sycophant-kubectl

Minimal Alpine + `kubectl` image. Used by chart-internal Jobs that need to
call the Kubernetes API — currently the post-install wait Job in
`charts/sycophant-tenant/templates/install-wait-job.yaml`.

## Why a custom image

Public kubectl images either lack a shell (rancher/kubectl is distroless) or
carry licensing / pinning friction (Bitnami's free catalog moves through
`bitnamilegacy/` with version-pinned tags that drift). A 6-line Dockerfile
gives us a deterministic, smaller alternative we control.

## Build + load (e2e)

`scripts/e2e.sh` builds this in `step_1_build` and loads it into k3d as
`sycophant-kubectl:local`. The chart template hardcodes the same tag with
`imagePullPolicy: IfNotPresent`, so kubelet finds the locally-imported
image without registry round-trips.

## Bumping kubectl version

1. Update the `KUBECTL_VERSION` `ARG` default in `Dockerfile`.
2. If the new version drifts from k3s/k3d's server version, verify kubectl
   skew compatibility (kubectl is officially +/-1 minor; basic operations
   like `create secret` tolerate wider skew).
3. Run the e2e — the image is rebuilt every run, no extra step needed.

# sycophant-caddy

Caddy for the cluster ingress (`charts/sycophant-ingress`), installed by
`sycoadm ingress up`.

## Why a custom image

The official Caddy image sets a file capability on the binary
(`setcap cap_net_bind_service=+ep`). The pod policy drops ALL capabilities
and forbids privilege escalation, and Linux refuses to exec a binary whose
file capabilities fall outside the bounding set. This image copies the
release binary with no file capability. Caddy listens on 8443, so it needs
none.

The release tarball is pinned by version and by the SHA-512 from the
release's `caddy_<version>_checksums.txt`; a mismatch fails the build.
Alpine's base carries the CA bundle Caddy uses to reach Let's Encrypt.

## Build + load

`sycoadm setup` from a repo checkout builds it as `sycophant-caddy:local`
and imports it into the k3d node. The chart uses the same tag with
`imagePullPolicy: IfNotPresent`, so it is never pulled.

## Bumping Caddy

1. Update `CADDY_VERSION` and both `CADDY_SHA512_*` from the new release's
   `caddy_<version>_checksums.txt`.
2. Check the release's `go.mod` certmagic version: `sycoadm ingress up`
   reads certmagic's `certificate obtained successfully` and
   `could not get certificate from issuer` log lines.

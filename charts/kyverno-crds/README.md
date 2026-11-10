# kyverno-crds

Vendored Kyverno CRDs. Sibling of the upstream `kyverno` chart.

## Why a separate chart

If Kyverno's own chart installed its CRDs, `helm uninstall kyverno` would
delete them, and with them every `ClusterPolicy` and `PolicyException` in the
cluster. This chart keeps the CRDs in their own Helm release, so operator
policies survive.

The CRDs also sit in Helm's install-only `crds/` directory, so even
`helm uninstall kyverno-crds` leaves them.

## Install

```sh
helm install kyverno-crds charts/kyverno-crds
```

Install it before `helm install kyverno ...`.

## Bump procedure

When bumping the Kyverno minor version:

1. `helm repo update kyverno`
2. `helm pull kyverno/kyverno --version <new> --untar --untardir /tmp/`
3. Render with CRDs on and split one file per CRD, named `<plural>.<group>.yaml`:
   ```sh
   helm template kyverno /tmp/kyverno \
     --set crds.install=true --set openreports.installCrds=false
   ```
   The CRDs come from two subcharts, both gated on `crds.install`: `crds`
   (kyverno.io, reports.kyverno.io, wgpolicyk8s.io) and the separate
   `kyverno-api` dependency (policies.kyverno.io). Both must be rendered.
4. Strip the five helm release labels the render stamps on every CRD:
   `app.kubernetes.io/{instance,managed-by,part-of,version}` and
   `helm.sh/chart`. They name the upstream release, but this chart owns the
   objects.
5. Strip every `description` field (see below). Everything else is verbatim.
6. Bump `Chart.yaml` `appVersion` and (semver-appropriate) `version`, and the
   version named in `values.yaml`.
7. Diff the `crds/` tree against the previous version, and check schema
   changes for breaking field renames.

## Why descriptions are stripped

A helm release is stored in a Secret, and Kubernetes caps a Secret at 1 MiB.
The full upstream CRD set is 5.6 MB of YAML, so `helm install` would fail with
it. The CEL policy types and their `Namespaced*` variants pushed it over.
Descriptions are about two thirds of that
text and carry no validation behaviour: they are the help text `kubectl
explain` prints. Dropping them takes the tree to 1.9 MB and changes nothing
the apiserver enforces. The cost is that `kubectl explain` on a Kyverno type
prints field names without prose; read the upstream docs instead.

Strip line-wise rather than by re-serialising the document, so the remaining
bytes stay identical to what upstream rendered and step 7's diff still means
something. Verify by parsing both the original and the stripped file and
asserting they are equal once descriptions are removed from the original.

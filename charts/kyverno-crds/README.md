# kyverno-crds

Vendored Kyverno CRDs. Sibling of the upstream `kyverno` chart.

## Why a separate chart

When `sycophant-quickstart` bundles Kyverno, `helm uninstall sycophant-quickstart`
would delete every CRD Kyverno owns — cascading into every `ClusterPolicy`,
`PolicyException`, etc. in the cluster. Splitting CRDs into a sibling chart
puts them in their own helm release so `helm uninstall` of the quickstart
leaves them registered. Operator policies survive. Matches the Linkerd /
Karpenter sibling-CRDs pattern called out by ADR 015.

The CRDs are also placed in helm's `crds/` directory, which is install-only
and never deleted by `helm uninstall` even of this chart — double-belted
retention.

## Install

```sh
helm install kyverno-crds charts/kyverno-crds
```

Run before `helm install kyverno ...`. The quickstart meta-chart handles the
ordering automatically via its `dependencies:` block.

## Bump procedure

When bumping the Kyverno minor version:

1. `helm repo update kyverno`
2. `helm pull kyverno/kyverno --version <new> --untar --untardir /tmp/`
3. Re-extract per-CRD files into `crds/`:
   ```sh
   helm template /tmp/kyverno | \
     awk '/^---$/ {if (have && f) { print buf > f } buf=""; have=0; f=""; next}
          /^kind: CustomResourceDefinition/ {have=1}
          have && /^  name: / { ... derive filename from $2 ... }
          { buf = buf $0 "\n" }'
   ```
4. Bump `Chart.yaml` `appVersion` and (semver-appropriate) `version`.
5. Diff the `crds/` tree against the previous version. Eyeball schema changes
   for breaking field renames; flag in the chart's CHANGELOG.

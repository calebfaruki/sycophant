#!/usr/bin/env bash
# Offline: renders the cluster chart with its CRDs and checks it ships no
# Kyverno, so `helm uninstall sycophant` leaves Kyverno, its CRDs and every
# user-written policy in place. No cluster.
#
#   bash tests/unit/cluster-chart/acceptance.sh
set -uo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
readonly HERE
source "$HERE/../lib.sh"

capture cluster helm template c "$ROOT/charts/sycophant-cluster" -n kyverno \
  --include-crds --api-versions policies.kyverno.io/v1/ValidatingPolicy
if [ "$(cat "$WORK/cluster.rc")" -ne 0 ]; then
  bad render "$(cat "$WORK/cluster.err")"
  summary
  exit 1
fi

# kyverno_parts: prints every rendered Deployment, StatefulSet, DaemonSet,
# webhook and Kyverno CRD.
kyverno_parts() {
  python3 - "$WORK/cluster.yaml" <<'PY'
import sys, yaml
for d in yaml.safe_load_all(open(sys.argv[1])):
    if not d:
        continue
    kind, name = d["kind"], d["metadata"]["name"]
    if kind == "CustomResourceDefinition" and name.endswith("kyverno.io"):
        print(f"{kind}/{name}")
    elif kind.endswith("WebhookConfiguration") or kind in (
            "Deployment", "StatefulSet", "DaemonSet"):
        print(f"{kind}/{name}")
PY
}

check="the cluster chart ships no Deployments, StatefulSets, DaemonSets,"
check+=" webhooks or Kyverno CRDs"
if ! found="$(kyverno_parts)"; then
  bad "$check" "could not read the render"
elif [ -z "$found" ]; then
  ok "$check"
else
  bad "$check" "it renders: $(tr '\n' ' ' <<<"$found")"
fi

summary

#!/usr/bin/env bash
# Smoke-check that chainsaw tests actually fail when sycophant's
# policies are removed. If a test still passes after the policy under
# it is deleted, the test is performative and must be fixed.
#
# Two mutation styles:
#   - SUBTRACTIVE: delete an existing cluster-scoped resource that the
#     chart ships (Kyverno ClusterPolicy, VAP). Restore by re-applying
#     the chart.
#   - ADDITIVE: apply a resource that should NOT exist (e.g. a CRB
#     granting the workspace SA cluster-admin). Restore by deleting it.
#
# Usage:
#   scripts/mutation-removal.sh workspace-vap        # subtractive
#   scripts/mutation-removal.sh tenant-naming        # subtractive
#   scripts/mutation-removal.sh protect-security     # subtractive
#   scripts/mutation-removal.sh tenant-perimeter     # subtractive
#   scripts/mutation-removal.sh workspace-sa-no-rbac # additive
#
# Exit code: 0 if mutation caused the expected failures, 1 if tests
# passed despite the mutation (performative bug), 2 on script error.
#
# Requires: kubectl, helm, chainsaw on PATH; cluster admin context.

set -euo pipefail

MUTATION="${1:?usage: $0 <workspace-vap|tenant-naming|protect-security|tenant-perimeter|workspace-sa-no-rbac>}"
REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
RELEASE_NAME="${RELEASE_NAME:-test}"
RELEASE_NAMESPACE="${RELEASE_NAMESPACE:-default}"

# Required: chainsaw binary on PATH. rc=127 (command not found) is
# distinct from rc=1 (tests failed); without this check the script
# would silently report a false safety net.
if ! command -v chainsaw >/dev/null 2>&1; then
  echo "!!! chainsaw not found on PATH; cannot validate mutation." >&2
  echo "!!! Install via scripts/install-test-deps.sh." >&2
  exit 2
fi

MUTATION_KIND="subtractive"
TARGET_KIND=""
TARGET_NAME=""
EXPECTED_BUCKET=""
ADDITIVE_MANIFEST=""

case "$MUTATION" in
  workspace-vap)
    TARGET_KIND="validatingadmissionpolicy"
    TARGET_NAME="cluster-workspace-pod-policy"
    EXPECTED_BUCKET="tests/integration/workspace-pod-shape"
    ;;
  tenant-naming)
    TARGET_KIND="clusterpolicy"
    TARGET_NAME="tenant-namespace-naming"
    EXPECTED_BUCKET="tests/integration/tenant-namespace-creation/tenant-deployer-bad-name-rejected"
    ;;
  protect-security)
    TARGET_KIND="clusterpolicy"
    TARGET_NAME="cluster-protect-security"
    EXPECTED_BUCKET="tests/integration/tenant-resource-protection"
    ;;
  tenant-perimeter)
    TARGET_KIND="clusterpolicy"
    TARGET_NAME="tenant-namespace-perimeter-label"
    EXPECTED_BUCKET="tests/integration/tenant-namespace-creation/cluster-admin-unlabeled-rejected"
    ;;
  workspace-sa-no-rbac)
    MUTATION_KIND="additive"
    EXPECTED_BUCKET="tests/integration/rbac-blast-radius/workspace-sa-no-verbs"
    # Grant the synthetic workspace SA cluster-admin; the test creates
    # the namespace + SA itself (the chart's Kyverno mutate only fires
    # when tenant-deployer is the actor, so we can't pre-create the ns
    # cluster-admin-style). The CRB references the SA-to-be; K8s resolves
    # at request time. Restore = delete the CRB.
    TARGET_KIND="clusterrolebinding"
    TARGET_NAME="mutation-removal-workspace-sa-admin"
    read -r -d '' ADDITIVE_MANIFEST <<'EOF' || true
apiVersion: rbac.authorization.k8s.io/v1
kind: ClusterRoleBinding
metadata:
  name: mutation-removal-workspace-sa-admin
subjects:
  - kind: ServiceAccount
    name: sa-chainsaw-ws
    namespace: tenant-chainsaw-rbac-ws-sa
roleRef:
  kind: ClusterRole
  name: cluster-admin
  apiGroup: rbac.authorization.k8s.io
EOF
    ;;
  *)
    echo "unknown mutation: $MUTATION" >&2
    exit 2
    ;;
esac

if [ "$MUTATION_KIND" = "subtractive" ]; then
  echo ">>> mutation-removal (subtractive): deleting ${TARGET_KIND}/${TARGET_NAME}"
  kubectl delete "$TARGET_KIND" "$TARGET_NAME" --ignore-not-found --wait=true --timeout=30s
else
  echo ">>> mutation-removal (additive): applying ${TARGET_KIND}/${TARGET_NAME} + setup"
  printf '%s' "$ADDITIVE_MANIFEST" | kubectl apply -f -
fi

echo ">>> running chainsaw against $EXPECTED_BUCKET (expecting failure)"
set +e
chainsaw test "$REPO_ROOT/$EXPECTED_BUCKET" \
  --config "$REPO_ROOT/tests/integration/.chainsaw.yaml"
CHAINSAW_RC=$?
set -e

if [ "$CHAINSAW_RC" -eq 0 ]; then
  echo "!!! PERFORMATIVE TEST DETECTED."
  echo "!!! Chainsaw passed despite the mutation."
  echo "!!! The tests in $EXPECTED_BUCKET do not actually exercise sycophant code."
  EXIT=1
else
  echo ">>> chainsaw correctly failed under mutation (rc=$CHAINSAW_RC)."
  EXIT=0
fi

if [ "$MUTATION_KIND" = "subtractive" ]; then
  echo ">>> restoring chart via helm template | kubectl apply"
  ( cd "$REPO_ROOT/charts/sycophant-quickstart" && helm dependency update >/dev/null )
  helm template "$RELEASE_NAME" "$REPO_ROOT/charts/sycophant-quickstart" \
    -n "$RELEASE_NAMESPACE" | kubectl apply -f - >/dev/null
else
  echo ">>> restoring: deleting ${TARGET_KIND}/${TARGET_NAME}"
  kubectl delete "$TARGET_KIND" "$TARGET_NAME" --ignore-not-found --wait=true --timeout=30s
fi

exit "$EXIT"

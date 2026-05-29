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
#   scripts/mutation-removal.sh workspace-vap            # subtractive
#   scripts/mutation-removal.sh tenant-naming            # subtractive
#   scripts/mutation-removal.sh protect-security         # subtractive
#   scripts/mutation-removal.sh tenant-perimeter         # subtractive
#   scripts/mutation-removal.sh tenant-tokenreview-crbs        # subtractive
#   scripts/mutation-removal.sh tokenreview-clusterrole-rules  # subtractive
#   scripts/mutation-removal.sh workspace-sa-no-rbac           # additive
#   scripts/mutation-removal.sh workspace-vap-rbac             # additive
#   scripts/mutation-removal.sh tightbeam-secret-name-allowlist # subtractive
#   scripts/mutation-removal.sh workspace-egress-cnp           # subtractive
#   scripts/mutation-removal.sh runtimeclass-gvisor            # subtractive
#
# Exit code: 0 if mutation caused the expected failures, 1 if tests
# passed despite the mutation (performative bug), 2 on script error.
#
# Requires: kubectl, helm, chainsaw on PATH; cluster admin context.

set -euo pipefail

MUTATION="${1:?usage: $0 <workspace-vap|tenant-naming|protect-security|tenant-perimeter|tenant-tokenreview-crbs|tokenreview-clusterrole-rules|workspace-sa-no-rbac|workspace-vap-rbac|tightbeam-secret-name-allowlist|workspace-egress-cnp|runtimeclass-gvisor>}"
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
TARGET_NS=""
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
  tenant-tokenreview-crbs)
    TARGET_KIND="clusterpolicy"
    TARGET_NAME="tenant-rolebinding-generator"
    EXPECTED_BUCKET="tests/integration/tenant-namespace-creation/tenant-tokenreview-crbs-generated"
    # Deleting tenant-rolebinding-generator removes all five rules,
    # including the two generate rules (3a, 3b) that produce the per-ns
    # ClusterRoleBindings binding cluster-{airlock,tightbeam}-tokenreview
    # to the per-tenant controller SAs. The new chainsaw test creates a
    # tenant ns via tenant-deployer and asserts both CRBs exist with the
    # right roleRef + subject; without the policy, neither CRB is
    # generated and the asserts time out. Collateral (PSA labels,
    # deployer RB, VAP binding) is restored by the helm-template reapply
    # below; no other chainsaw bucket runs concurrently with this script.
    ;;
  tokenreview-clusterrole-rules)
    TARGET_KIND="clusterrole"
    TARGET_NAME="cluster-airlock-tokenreview"
    EXPECTED_BUCKET="tests/integration/tenant-namespace-creation/tenant-tokenreview-crbs-generated"
    # Deleting cluster-airlock-tokenreview makes the
    # assert-tokenreview-clusterrole-rules step fail on resource-not-found.
    # Validates the rule-drift assertion in isolation from the CRB-existence
    # assertions (different mutation, same test bucket). Restore reapplies
    # the chart; the per-tenant CRBs continue to reference the role name
    # while it is missing, but that does not surface a test regression here.
    ;;
  runtimeclass-gvisor)
    TARGET_KIND="runtimeclass"
    TARGET_NAME="gvisor"
    EXPECTED_BUCKET="tests/integration/cluster-resources/runtimeclass-gvisor-handler"
    # Deleting the RuntimeClass makes the chainsaw assert fail on
    # resource-not-found. Restore reapplies the sycophant-quickstart
    # umbrella, which depends on sycophant-gvisor and re-creates the
    # RuntimeClass.
    ;;
  workspace-egress-cnp)
    TARGET_KIND="ciliumnetworkpolicy"
    TARGET_NAME="workspace-egress"
    TARGET_NS="${TARGET_NS:-e2e-test}"
    EXPECTED_BUCKET="tests/integration/workspace-pod-shape/workspace-egress-dns-allowlist"
    # Deleting workspace-egress removes the L7 DNS allow-list; the
    # chainsaw bucket asserts the CNP's matchName entries, so deletion
    # makes the assert step fail on resource-not-found. Restore reapplies
    # the tenant chart, which re-creates the CNP from
    # `charts/sycophant-tenant/templates/workspace-netpol.yaml`. NB: the
    # tenant-chart restore path differs from the umbrella restore used by
    # other buckets; the helm-template at the script's bottom uses
    # sycophant-quickstart, which does NOT depend on sycophant-tenant. A
    # tenant chart restore is handled inline below (search for
    # workspace-egress-cnp in the restore branch).
    ;;
  tightbeam-secret-name-allowlist)
    TARGET_KIND="validatingadmissionpolicy"
    TARGET_NAME="cluster-tightbeam-secret-name-allowlist"
    EXPECTED_BUCKET="tests/integration/sa-permission-bounds/tightbeam-secret-name-allowlist"
    # Deleting the VAP removes the structural allow-list. The
    # `forbidden-name-rejected` and `forbidden-name-with-generate-name-rejected`
    # steps expect admission rejection on `evil-secret` / `evil-*`; without
    # the VAP, both Secret creates succeed (RBAC alone permits any name),
    # the `$error != null` assertions fail, and the test bucket goes red.
    ;;
  workspace-sa-no-rbac)
    MUTATION_KIND="additive"
    EXPECTED_BUCKET="tests/integration/sa-permission-bounds/workspace-sa-no-verbs"
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
  workspace-vap-rbac)
    MUTATION_KIND="additive"
    EXPECTED_BUCKET="tests/integration/tenant-resource-protection/workspace-admission-rule-immutable"
    # Bind tenant-deployer to cluster-chart-admin (which already has VAP
    # edit verbs). tenant-deployer is one of the identities the chainsaw
    # test asserts as Forbidden — with this binding, the delete succeeds
    # at RBAC, the Forbidden assertion fails, and the test goes RED.
    # Restore = delete the CRB.
    TARGET_KIND="clusterrolebinding"
    TARGET_NAME="mutation-removal-tenant-deployer-chart-admin"
    read -r -d '' ADDITIVE_MANIFEST <<'EOF' || true
apiVersion: rbac.authorization.k8s.io/v1
kind: ClusterRoleBinding
metadata:
  name: mutation-removal-tenant-deployer-chart-admin
subjects:
  - kind: ServiceAccount
    name: tenant-deployer
    namespace: infra
roleRef:
  kind: ClusterRole
  name: cluster-chart-admin
  apiGroup: rbac.authorization.k8s.io
EOF
    ;;
  *)
    echo "unknown mutation: $MUTATION" >&2
    exit 2
    ;;
esac

if [ "$MUTATION_KIND" = "subtractive" ]; then
  echo ">>> mutation-removal (subtractive): deleting ${TARGET_KIND}/${TARGET_NAME}${TARGET_NS:+ -n $TARGET_NS}"
  if [ -n "$TARGET_NS" ]; then
    kubectl delete "$TARGET_KIND" "$TARGET_NAME" -n "$TARGET_NS" --ignore-not-found --wait=true --timeout=30s
  else
    kubectl delete "$TARGET_KIND" "$TARGET_NAME" --ignore-not-found --wait=true --timeout=30s
  fi
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
  # pipefail is set at script scope (L28). helm-template failures already
  # abort. This guard exists for the case where helm exits 0 but emits
  # empty/near-empty output — every template excluded by a Capabilities
  # gate, all resources gated off by values, or rendered down to only
  # `---` separators. Without the guard, kubectl applies nothing, exits
  # 0, and the script reports a successful restore against a still-broken
  # cluster.
  #
  # --api-versions must list every CRD apiVersion that any chart template
  # gates on via `.Capabilities.APIVersions.Has`. Add to this list when
  # introducing a new Capabilities-gated template.
  RENDERED="$(mktemp)"
  trap 'rm -f "$RENDERED"' EXIT
  helm template "$RELEASE_NAME" "$REPO_ROOT/charts/sycophant-quickstart" \
    -n "$RELEASE_NAMESPACE" \
    --api-versions kyverno.io/v1/ClusterPolicy \
    > "$RENDERED"
  if [ ! -s "$RENDERED" ] || ! grep -q '^kind:' "$RENDERED"; then
    echo "!!! helm template produced no Kubernetes resources; restore aborted." >&2
    echo "!!! Rendered output: $RENDERED" >&2
    exit 2
  fi
  kubectl apply -f "$RENDERED" >/dev/null

  # Tenant-chart restore: the umbrella above is sycophant-quickstart,
  # which depends on sycophant-cluster but NOT sycophant-tenant. For
  # buckets whose target lives in the tenant chart (TARGET_NS set), we
  # additionally render+apply the relevant tenant-chart template into
  # the same namespace. e2e values reproduce the tenant install used by
  # scripts/e2e.sh:331.
  if [ -n "$TARGET_NS" ]; then
    echo ">>> tenant-chart restore: helm template sycophant-tenant -n $TARGET_NS"
    TENANT_RENDERED="$(mktemp)"
    trap 'rm -f "$RENDERED" "$TENANT_RENDERED"' EXIT
    helm template "$TARGET_NS" "$REPO_ROOT/charts/sycophant-tenant" \
      -n "$TARGET_NS" \
      -f "$REPO_ROOT/docs/e2e/values.yaml" \
      --set "clients.${TENANT_CLIENT_NAME:-calebs-pixel}.workspaces={hello-world}" \
      > "$TENANT_RENDERED"
    if [ ! -s "$TENANT_RENDERED" ] || ! grep -q '^kind:' "$TENANT_RENDERED"; then
      echo "!!! tenant helm template produced no Kubernetes resources; restore aborted." >&2
      exit 2
    fi
    kubectl apply -f "$TENANT_RENDERED" >/dev/null
  fi
else
  echo ">>> restoring: deleting ${TARGET_KIND}/${TARGET_NAME}"
  kubectl delete "$TARGET_KIND" "$TARGET_NAME" --ignore-not-found --wait=true --timeout=30s
fi

exit "$EXIT"

#!/usr/bin/env bash
# Exercise the hobbyist quickstart on a fresh k3d cluster. Asserts that
# helm install lands cleanly AND that `helm uninstall` leaves Kyverno
# policies + their CRDs intact (the soft-rebuild invariant).
#
# Independent from scripts/e2e.sh — the e2e is the DevOps piece-by-piece
# validation path; this script is the hobbyist one-command validation path.
set -euo pipefail

REPO_ROOT="$(git rev-parse --show-toplevel)"
CLUSTER_NAME="${CLUSTER_NAME:-sycophant-quickstart-test}"
RELEASE_NAME="${RELEASE_NAME:-qs}"

step() { printf '\n\033[1;36m==> %s\033[0m\n' "$*"; }
ok()   { printf '\033[1;32m ✓\033[0m %s\n' "$*"; }
warn() { printf '\033[1;33m ⚠\033[0m %s\n' "$*" >&2; }

# Kill leftover sycophant processes from prior runs (leaked cargo test
# binaries, abandoned kubectl port-forwards, etc.) before doing anything.
"$REPO_ROOT/scripts/kill-orphans.sh"

cleanup() {
  if [ "${KEEP_CLUSTER:-}" = "1" ]; then
    warn "KEEP_CLUSTER=1; leaving $CLUSTER_NAME alive for inspection"
    return
  fi
  k3d cluster delete "$CLUSTER_NAME" >/dev/null 2>&1 || true
}
trap cleanup EXIT

step "Step 1: Fresh k3d cluster"
k3d cluster delete "$CLUSTER_NAME" >/dev/null 2>&1 || true
k3d cluster create "$CLUSTER_NAME" \
  --k3s-arg "--flannel-backend=none@server:*" \
  --k3s-arg "--disable-network-policy@server:*" \
  --wait >/dev/null
ok "k3d cluster $CLUSTER_NAME created"

step "Step 2: Resolve dependencies"
( cd "$REPO_ROOT/charts/sycophant-quickstart" && helm dependency update >/dev/null )
ok "helm dep update complete"

step "Step 3: Install quickstart"
helm install "$RELEASE_NAME" "$REPO_ROOT/charts/sycophant-quickstart" \
  --wait --timeout=10m >/dev/null
ok "quickstart installed"

step "Step 4: Verify CRDs Established"
for crd in clusterpolicies.kyverno.io chambers.sycophant.md; do
  kubectl wait --for=condition=Established "crd/$crd" --timeout=30s >/dev/null
  ok "crd/$crd Established"
done

step "Step 5: Cilium FQDN egress enforcement (chamber-shaped CNP)"
# Mirrors the chamber CNP shape from
# charts/sycophant-tenant/templates/airlock-chamber-netpol.yaml:52-86.
# Proves the security promise that chambers depend on -- toFQDNs allowlist
# actually blocks traffic to non-allowlisted hosts.

# Baseline: probe pod with NO policy yet. curl google.com must succeed.
# (Catches false-positive "google.com blocked" failures caused by broken
# networking rather than policy enforcement.)
kubectl run fqdn-probe --image=nicolaka/netshoot --restart=Never \
  --labels=sycophant.md/chamber=fqdn-probe -- sleep 600 >/dev/null
kubectl wait pod/fqdn-probe --for=condition=Ready --timeout=300s >/dev/null
if ! kubectl exec fqdn-probe -- curl -sS --max-time 8 -o /dev/null https://www.google.com 2>/dev/null; then
  kubectl delete pod fqdn-probe --force --grace-period=0 >/dev/null 2>&1
  warn "baseline curl to google.com failed BEFORE any policy applied -- networking is broken, not a policy test"
  exit 1
fi
ok "baseline: probe pod can reach google.com (no policy)"

# Apply the chamber-shaped CNP: allow only github.com:443 + DNS for github.com.
kubectl apply -f - >/dev/null <<'EOF'
apiVersion: cilium.io/v2
kind: CiliumNetworkPolicy
metadata:
  name: fqdn-probe-policy
spec:
  endpointSelector:
    matchLabels:
      sycophant.md/chamber: fqdn-probe
  egress:
    - toEndpoints:
        - matchLabels:
            io.kubernetes.pod.namespace: kube-system
            k8s-app: kube-dns
      toPorts:
        - ports:
            - { port: "53", protocol: UDP }
            - { port: "53", protocol: TCP }
          rules:
            dns:
              - matchName: github.com
              - matchPattern: "*.github.com"
    - toFQDNs:
        - matchName: github.com
        - matchPattern: "*.github.com"
      toPorts:
        - ports:
            - { port: "443", protocol: TCP }
EOF
# Give Cilium a beat to compile + push the policy to the datapath.
sleep 5

# Four assertions.
# Use trailing-dot absolute names so the glibc resolver skips the search-path
# expansion (default.svc.cluster.local etc.). Without the dot, glibc tries
# `github.com.default.svc.cluster.local` first; Cilium's L7 DNS proxy refuses
# those names (not in the allowlist), and the resolver fails before ever
# trying bare github.com. Real chambers either don't have a search path or
# use absolute names; the test mirrors that.
#
# For the curl assertions, letting curl do its own DNS lookup is critical:
# Cilium's FQDN policy tracks which IPs were resolved from allowlisted DNS
# queries; --resolve would bypass DNS and Cilium wouldn't know the IP
# belongs to github.com.
if ! kubectl exec fqdn-probe -- getent hosts github.com. >/dev/null 2>&1; then
  warn "DNS for github.com. failed (should succeed via L7 allowlist)"
  exit 1
fi
ok "DNS for github.com. resolved"

github_code=$(kubectl exec fqdn-probe -- curl -sS --max-time 8 \
  -o /dev/null -w '%{http_code}' 'https://github.com./' 2>&1 || true)
case "$github_code" in
  200|301|302) ok "curl https://github.com → $github_code (allowed by FQDN allowlist)" ;;
  *) warn "curl https://github.com returned '$github_code' (expected 200/301/302)"; exit 1 ;;
esac

if kubectl exec fqdn-probe -- curl -sS --max-time 8 \
     -o /dev/null 'https://www.google.com./' 2>/dev/null; then
  warn "curl https://www.google.com SUCCEEDED -- FQDN policy is NOT enforcing"
  exit 1
fi
ok "curl https://www.google.com blocked (not in FQDN allowlist)"

if kubectl exec fqdn-probe -- getent hosts evil.example.com. 2>/dev/null; then
  warn "DNS for evil.example.com. SUCCEEDED -- L7 DNS proxy is NOT filtering"
  exit 1
fi
ok "DNS for evil.example.com. blocked (L7 DNS allowlist working)"

kubectl delete cnp fqdn-probe-policy --ignore-not-found >/dev/null
kubectl delete pod fqdn-probe --force --grace-period=0 --ignore-not-found >/dev/null 2>&1

step "Step 6: helm uninstall preserves user-data CRDs and ClusterPolicy"

# Plant a witness ClusterPolicy authored by the user (not by helm).
# If it survives helm uninstall, the sibling kyverno-crds chart is
# doing its job.
kubectl apply -f - >/dev/null <<'EOF'
apiVersion: kyverno.io/v1
kind: ClusterPolicy
metadata:
  name: qs-uninstall-witness
spec:
  validationFailureAction: Audit
  rules:
    - name: noop
      match:
        any:
          - resources:
              kinds: [Pod]
      validate:
        message: noop
        pattern:
          metadata:
            name: "?*"
EOF
ok "user-authored ClusterPolicy planted"

# Capture diagnostics if the assertions below fail (cleanup trap nukes
# the cluster on exit).
DIAG="/tmp/quickstart-test-uninstall-diag.$$"
trap 'rc=$?; if [ $rc -ne 0 ]; then kubectl get all,clusterpolicy,validatingwebhookconfiguration,mutatingwebhookconfiguration -A > "$DIAG" 2>&1 || true; warn "diagnostics captured at $DIAG"; fi; cleanup' EXIT

helm uninstall "$RELEASE_NAME" --timeout=5m >/dev/null
ok "helm uninstall completed cleanly (no --wait: namespace finalization races with"
ok "                                  Cilium teardown on k3d; tracked separately)"

# CRDs from the sibling kyverno-crds chart must survive.
kubectl get crd clusterpolicies.kyverno.io >/dev/null
ok "clusterpolicies.kyverno.io CRD survived uninstall"

# CRDs from the sycophant-cluster chart's crds/ dir must also survive.
kubectl get crd chambers.sycophant.md >/dev/null
ok "chambers.sycophant.md CRD survived uninstall"

# The witness ClusterPolicy (user-authored CR) must survive.
kubectl get clusterpolicy qs-uninstall-witness >/dev/null
ok "user-authored ClusterPolicy survived uninstall"

# No orphaned Kyverno webhook configs -- proves the pre-delete hook ran.
orphans=$(kubectl get validatingwebhookconfiguration,mutatingwebhookconfiguration \
  -l webhook.kyverno.io/managed-by=kyverno --no-headers 2>/dev/null | wc -l | tr -d ' ')
if [ "$orphans" != "0" ]; then
  warn "$orphans orphaned Kyverno webhook config(s) -- pre-delete hook did not clean up"
  exit 1
fi
ok "no orphaned Kyverno webhook configs"

kubectl delete clusterpolicy qs-uninstall-witness --ignore-not-found >/dev/null

printf '\n\033[1;32m==> quickstart-test passed\033[0m\n'

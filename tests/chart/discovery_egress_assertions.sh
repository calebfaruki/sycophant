#!/usr/bin/env bash
# Acceptance test — chart egress + RBAC for the discovery-off-controller change.
#
# Renders the sycophant-tenant chart with `helm template --show-only` (no
# cluster needed) and asserts three things via python3/pyyaml:
#
#   (a) the discovery-Job CiliumNetworkPolicy exists, selects ONLY the discovery
#       discriminator label (sycophant.md/job: discovery) and not the shared
#       airlock-job component, and permits egress on 443 + 5000 with an L7 DNS
#       rule (rules.dns).
#   (b) toolset-ctrl-egress permits ONLY kube-apiserver + kube-dns — no world /
#       registry / 443 / 5000.
#   (c) the controller Role has no pods / pods/log resource.
#
# Exit non-zero if any REQUIRED check fails — run as a guard against regressing
# the discovery-off-controller egress split.

set -uo pipefail

REPO_ROOT="$(git rev-parse --show-toplevel)"
CHART="$REPO_ROOT/charts/sycophant-tenant"
NS=chart-test
FAIL=0
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

render() {
  # $1 = template path under templates/, $2 = output file. Empty file on error.
  helm template "$NS" "$CHART" --show-only "templates/$1" -n "$NS" >"$2" 2>/dev/null || true
}

# ---- (a) discovery-Job netpol: RED today ---------------------------------
echo "== (a) discovery-job netpol (expected RED today) =="
render discovery-job-netpol.yaml "$WORK/disc.yaml"
if [ ! -s "$WORK/disc.yaml" ]; then
  echo "FAIL (a): templates/discovery-job-netpol.yaml does not render (missing)"
  FAIL=1
else
  python3 - "$WORK/disc.yaml" <<'PY' || FAIL=1
import sys, yaml
docs = [d for d in yaml.safe_load_all(open(sys.argv[1])) if d]
cnp = next((d for d in docs if d.get("kind") == "CiliumNetworkPolicy"), None)
assert cnp, "no CiliumNetworkPolicy rendered"
sel = (cnp.get("spec", {}).get("endpointSelector", {}) or {}).get("matchLabels", {}) or {}
assert sel.get("sycophant.md/job") == "discovery", \
    f"endpointSelector must select sycophant.md/job=discovery, got {sel}"
assert sel.get("app.kubernetes.io/component") != "airlock-job", \
    "discovery netpol must NOT select all airlock-job workers"
egress = cnp.get("spec", {}).get("egress", []) or []
ports, has_dns = set(), False
for rule in egress:
    for tp in rule.get("toPorts", []) or []:
        for p in tp.get("ports", []) or []:
            ports.add(str(p.get("port")))
        if tp.get("rules", {}).get("dns"):
            has_dns = True
assert "443" in ports, f"egress must permit 443, got ports {sorted(ports)}"
assert "5000" in ports, f"egress must permit 5000, got ports {sorted(ports)}"
assert has_dns, "egress must carry an L7 DNS rule (rules.dns) so registry hostnames resolve"
print("PASS (a): discovery netpol scoped to discovery pods with 443+5000+L7 DNS")
PY
fi

# ---- (b) controller egress unchanged: GREEN-GUARD ------------------------
echo "== (b) toolset-ctrl-egress unchanged (GREEN-GUARD) =="
render toolset-ctrl-netpol.yaml "$WORK/ctrl.yaml"
if [ ! -s "$WORK/ctrl.yaml" ]; then
  echo "FAIL (b): templates/toolset-ctrl-netpol.yaml did not render"
  FAIL=1
else
  python3 - "$WORK/ctrl.yaml" <<'PY' || FAIL=1
import sys, yaml
docs = [d for d in yaml.safe_load_all(open(sys.argv[1])) if d]
egress_cnp = next(
    (d for d in docs
     if d.get("kind") == "CiliumNetworkPolicy"
     and d.get("metadata", {}).get("name") == "toolset-ctrl-egress"),
    None,
)
assert egress_cnp, "toolset-ctrl-egress CNP not found"
entities, ports = set(), set()
for rule in egress_cnp.get("spec", {}).get("egress", []) or []:
    for e in rule.get("toEntities", []) or []:
        entities.add(e)
    assert "toCIDR" not in rule and "toCIDRSet" not in rule, \
        "controller egress must not add any CIDR (registry/world) rule"
    for tp in rule.get("toPorts", []) or []:
        for p in tp.get("ports", []) or []:
            ports.add(str(p.get("port")))
assert entities <= {"kube-apiserver"}, \
    f"controller egress entities must be kube-apiserver only, got {entities}"
assert "world" not in entities, "controller must never egress to world"
assert "443" not in ports and "5000" not in ports, \
    f"controller must not open registry ports, got {sorted(ports)}"
print("PASS (b): toolset-ctrl-egress is kube-apiserver + kube-dns only")
PY
fi

# ---- (c) controller Role has no pods / pods/log: GREEN-GUARD -------------
echo "== (c) controller Role omits pods/pods/log (GREEN-GUARD) =="
render toolset-ctrl-rbac.yaml "$WORK/rbac.yaml"
if [ ! -s "$WORK/rbac.yaml" ]; then
  echo "FAIL (c): templates/toolset-ctrl-rbac.yaml did not render"
  FAIL=1
else
  python3 - "$WORK/rbac.yaml" <<'PY' || FAIL=1
import sys, yaml
docs = [d for d in yaml.safe_load_all(open(sys.argv[1])) if d]
role = next(
    (d for d in docs
     if d.get("kind") == "Role" and d.get("metadata", {}).get("name") == "toolset-ctrl"),
    None,
)
assert role, "toolset-ctrl Role not found"
resources = set()
for rule in role.get("rules", []) or []:
    for r in rule.get("resources", []) or []:
        resources.add(r)
assert "pods" not in resources, "controller Role must not grant pods"
assert "pods/log" not in resources, "controller Role must not grant pods/log"
print("PASS (c): controller Role omits pods and pods/log")
PY
fi

echo
if [ "$FAIL" -ne 0 ]; then
  echo "RESULT: FAIL (expected today — (a) is red until the discovery netpol lands)"
  exit 1
fi
echo "RESULT: PASS"
exit 0

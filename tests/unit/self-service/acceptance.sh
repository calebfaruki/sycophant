#!/usr/bin/env bash
# Offline: renders the cluster chart with the self-service setting off and on
# and checks each render. No cluster.
#
#   bash tests/unit/self-service/acceptance.sh
set -uo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
readonly HERE
source "$HERE/../lib.sh"
cp -R "$HERE/." "$WORK/"
CHART="$ROOT/charts/sycophant-cluster"
readonly CHART

render() {
  helm template c "$CHART" -n sycophant-system \
    --api-versions policies.kyverno.io/v1/ValidatingPolicy "$@" 2>&1
}
render >"$WORK/off.yaml" || {
  bad default_render "$(helm_err <"$WORK/off.yaml")"
  summary
  exit 1
}
OPERATOR="https://sycophant-dex.sycophant-system.svc.cluster.local:5556/dex#operator" # editorconfig-checker-disable-line
OPERATOR_ERR="selfService.operator must be set and start with the IdP"
OPERATOR_ERR+=" issuer prefix"
render --set selfService.enabled=true \
  --set-string "selfService.operator=$OPERATOR" \
  >"$WORK/on.yaml" || {
  bad setting_on_render "$(helm_err <"$WORK/on.yaml")"
  summary
  exit 1
}

# Parsed documents, not a text search: comments in the default render name
# some of these objects.
t_setting_off_renders_no_self_service_object() {
  local verdict
  verdict="$(
    python3 - "$WORK/off.yaml" "$WORK/on.yaml" 2>&1 <<'PY'
import sys, yaml
# Must name every object the setting adds. The off-render check sees only these.
want = {tuple(l.split()) for l in """
Role tenant-admins
RoleBinding tenant-admins
ValidatingAdmissionPolicy tenant-admins-system-configmaps
Namespace sycophant-entitlements
ServiceAccount entitlement-writer
Role entitlement-writer
RoleBinding entitlement-writer
ValidatingPolicy tenant-command-entitlement
GeneratingPolicy provision-tenant-owner
ValidatingAdmissionPolicy tenant-no-service-account-token
ValidatingAdmissionPolicy dex-password-id-is-name
ServiceAccount dex-password-writer
Role dex-password-writer
RoleBinding dex-password-writer
CronJob reap-tenant-files
ServiceAccount reap-tenant-files
ClusterRole reap-tenant-files
ClusterRoleBinding reap-tenant-files
Role reap-tenant-files
RoleBinding reap-tenant-files
ValidatingAdmissionPolicy reap-tenant-files-namespace-scope
CiliumNetworkPolicy reap-tenant-files-egress
""".splitlines() if l.strip()}
policies = {n for k, n in want if k == "ValidatingAdmissionPolicy"}
def shipped(path):
    docs = [d for d in yaml.safe_load_all(open(path)) if d]
    names = {(d["kind"], d["metadata"]["name"]) for d in docs}
    bound = {d["spec"]["policyName"] for d in docs
            if d["kind"] == "ValidatingAdmissionPolicyBinding"
              and d["spec"].get("policyName") in policies}
    return names, bound
off, off_bound = shipped(sys.argv[1])
on, on_bound = shipped(sys.argv[2])
problems = []
if want & off:
    problems.append(f"default render ships {sorted(want & off)}")
if off_bound:
    problems.append(f"default render binds {sorted(off_bound)}")
if want - on:
    problems.append(f"setting-on render lacks {sorted(want - on)}")
if policies - on_bound:
    problems.append(
        f"setting-on render does not bind {sorted(policies - on_bound)}")
print("; ".join(problems))
PY
  )"
  if [ -n "$verdict" ]; then
    bad setting_off_renders_no_self_service_object "$verdict"
  else
    ok setting_off_renders_no_self_service_object
  fi
}

t_operator_fails_render() {
  local name="$1" out
  shift
  if out="$(render --set selfService.enabled=true "$@")"; then
    bad "$name" "rendered"
  elif [[ "$out" != *"$OPERATOR_ERR"* ]]; then
    bad "$name" "failed for another reason: $(helm_err <<<"$out")"
  else
    ok "$name"
  fi
}

t_setting_on_maps_one_constant_group() {
  local verdict
  verdict="$(
    python3 - "$WORK/on.yaml" 2>&1 <<'PY'
import sys, yaml
cm = next(d for d in yaml.safe_load_all(open(sys.argv[1]))
          if d and "auth-config.yaml" in d.get("data", {}))
doc = yaml.safe_load(cm["data"]["auth-config.yaml"])
groups = doc["jwt"][0]["claimMappings"].get("groups")
expr = (groups or {}).get("expression", "")
if groups != {"expression": expr}:
    print(f"groups must hold one expression and no claim or prefix: {groups}")
elif expr != "'sycophant:tenant-admins'":
    print(
        "the group expression must be the literal "
        f"'sycophant:tenant-admins': {expr}")
PY
  )"
  if [ -n "$verdict" ]; then
    bad setting_on_maps_one_constant_group "$verdict"
  else
    ok setting_on_maps_one_constant_group
  fi
}

t_dex_password_id_must_be_its_name() {
  python3 - "$WORK/on.yaml" "$WORK/dex-password/password-policy.yaml" <<'PY'
import sys, yaml
docs = [d for d in yaml.safe_load_all(open(sys.argv[1])) if d]
keep = [d for d in docs
        if (d["kind"] == "ValidatingAdmissionPolicy"
            and d["metadata"]["name"] == "dex-password-id-is-name")
        or (d["kind"] == "ValidatingAdmissionPolicyBinding"
            and d["spec"].get("policyName") == "dex-password-id-is-name")]
yaml.safe_dump_all(keep, open(sys.argv[2], "w"))
PY
  expect dex-password pw-valid ""
  expect dex-password pw-id-is-username \
    "a Dex Password's userID must equal its metadata.name"
}

# Kyverno compiles its own policy kinds when it loads them, so a CEL type
# error fails here the way it fails at helm upgrade. Native VAPs are left out:
# kyverno apply compiles those only against a matching resource.
t_setting_on_kyverno_policies_compile() {
  local errors out
  mkdir -p "$WORK/kyverno-policies"
  python3 -c '
import sys, yaml
for i, d in enumerate(d for d in yaml.safe_load_all(sys.stdin) if d):
    if d["apiVersion"].split("/")[0] in ("policies.kyverno.io", "kyverno.io"):
        n = d["metadata"]["name"]
        yaml.safe_dump(d, open(f"{sys.argv[1]}/{i:03d}-{n}.yaml", "w"))
' "$WORK/kyverno-policies" <"$WORK/on.yaml"
  printf '%s\n' 'apiVersion: v1' 'kind: ConfigMap' \
    'metadata: {name: probe, namespace: default}' >"$WORK/probe-configmap.yaml"
  errors=""
  for f in "$WORK"/kyverno-policies/*.yaml; do
    out="$(kyverno apply "$f" --resource "$WORK/probe-configmap.yaml" 2>&1)"
    if [[ "$out" == *"failed to compile"* ]] ||
      [[ "$out" != *"Applying "[1-9]* ]]; then
      errors+=" $(basename "$f" .yaml)"
    fi
  done
  if [ -z "$(ls "$WORK"/kyverno-policies)" ]; then
    bad setting_on_kyverno_policies_compile "the render holds no Kyverno policy"
  elif [ -n "$errors" ]; then
    bad setting_on_kyverno_policies_compile \
      "these policies do not compile:$errors"
  else
    ok setting_on_kyverno_policies_compile
  fi
}

t_owner_grant_fires_on_the_marker() {
  local verdict
  verdict="$(
    python3 - "$WORK/on.yaml" 2>&1 <<'PY'
import json, sys, yaml
docs = [d for d in yaml.safe_load_all(open(sys.argv[1])) if d]
pol = (next((d for d in docs if d["kind"] == "GeneratingPolicy"
              and d["metadata"]["name"] == "provision-tenant-owner"), None))
problems = []
if pol is None:
    print("no GeneratingPolicy provision-tenant-owner")
    sys.exit()
mc = pol["spec"].get("matchConstraints") or {}
rules = mc.get("resourceRules") or []
if ([(r.get("resources"), r.get("operations"), r.get("resourceNames"))
      for r in rules] != [(["configmaps"], ["CREATE"],
                            ["provision-tenant-owner"])]):
    problems.append(
        f"must match only CREATE of configmap provision-tenant-owner: {rules}")
labels = (mc.get("namespaceSelector") or {}).get("matchLabels") or {}
if labels.get("app.kubernetes.io/part-of") != "sycophant-tenant":
    problems.append(
        f"must select only tenant namespaces: {mc.get('namespaceSelector')}")
if "sh.helm.release" in json.dumps(pol):
    problems.append("reads Helm's release record")
print("; ".join(problems))
PY
  )"
  if [ -n "$verdict" ]; then
    bad owner_grant_fires_on_the_marker "$verdict"
  else
    ok owner_grant_fires_on_the_marker
  fi
}

t_setting_off_renders_no_self_service_object
t_operator_fails_render setting_on_without_operator_fails_render
t_operator_fails_render setting_on_with_unprefixed_operator_fails_render \
  --set-string "selfService.operator=https://other.example.com/dex#operator"
t_setting_on_maps_one_constant_group
t_dex_password_id_must_be_its_name
t_setting_on_kyverno_policies_compile
t_owner_grant_fires_on_the_marker

summary

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

# Runs the check on stdin against both renders. The check defines
# problems(docs, mode) and returns what is wrong.
each_mode() {
  local verdict
  verdict="$(
    python3 - "$WORK/off.yaml" "$WORK/on.yaml" 2>&1 <<PY
import json, sys, yaml
CLEANER = "reap-tenant-files"
$(cat)
def run(path, mode):
    docs = [d for d in yaml.safe_load_all(open(path)) if d]
    return [f"{mode} render: {p}" for p in problems(docs, mode)]
print("; ".join(run(sys.argv[1], "off") + run(sys.argv[2], "on")))
PY
  )"
  if [ -n "$verdict" ]; then
    bad "$1" "$verdict"
  else
    ok "$1"
  fi
}

# Parsed documents, not a text search: comments in the default render name
# some of these objects.
t_setting_off_renders_no_self_service_object() {
  each_mode setting_off_renders_no_self_service_object <<'PY'
# Must name every object the setting adds. The off-render check sees only these.
WANT = {tuple(l.split()) for l in """
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
Role reap-tenant-files
RoleBinding reap-tenant-files
""".splitlines() if l.strip()}
POLICIES = {n for k, n in WANT if k == "ValidatingAdmissionPolicy"}
def problems(docs, mode):
    names = {(d["kind"], d["metadata"]["name"]) for d in docs}
    bound = {
        d["spec"]["policyName"] for d in docs
        if d["kind"] == "ValidatingAdmissionPolicyBinding"
        and d["spec"].get("policyName") in POLICIES}
    if mode == "off":
        return ([f"ships {sorted(WANT & names)}"] * bool(WANT & names)
                + [f"binds {sorted(bound)}"] * bool(bound))
    return ([f"lacks {sorted(WANT - names)}"] * bool(WANT - names)
            + [f"does not bind {sorted(POLICIES - bound)}"]
            * bool(POLICIES - bound))
PY
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
  each_mode setting_on_maps_one_constant_group <<'PY'
def problems(docs, mode):
    if mode == "off":
        return []
    cm = next(d for d in docs if "auth-config.yaml" in d.get("data", {}))
    doc = yaml.safe_load(cm["data"]["auth-config.yaml"])
    groups = doc["jwt"][0]["claimMappings"].get("groups")
    expr = (groups or {}).get("expression", "")
    if groups != {"expression": expr}:
        return [f"groups must hold one expression and no claim or prefix: "
                f"{groups}"]
    if expr != "'sycophant:tenant-admins'":
        return ["the group expression must be the literal "
                f"'sycophant:tenant-admins': {expr}"]
    return []
PY
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
  each_mode owner_grant_fires_on_the_marker <<'PY'
def problems(docs, mode):
    if mode == "off":
        return []
    pol = next((d for d in docs if d["kind"] == "GeneratingPolicy"
                and d["metadata"]["name"] == "provision-tenant-owner"), None)
    if pol is None:
        return ["no GeneratingPolicy provision-tenant-owner"]
    out = []
    mc = pol["spec"].get("matchConstraints") or {}
    rules = mc.get("resourceRules") or []
    want = [(["configmaps"], ["CREATE"], ["provision-tenant-owner"])]
    got = [
        (r.get("resources"), r.get("operations"), r.get("resourceNames"))
        for r in rules]
    if got != want:
        out.append(
            f"must match only CREATE of configmap {want[0][2][0]}: {rules}")
    selector = mc.get("namespaceSelector") or {}
    labels = selector.get("matchLabels") or {}
    if labels.get("app.kubernetes.io/part-of") != "sycophant-tenant":
        out.append(f"must select only tenant namespaces: {selector}")
    if "sh.helm.release" in json.dumps(pol):
        out.append("reads Helm's release record")
    return out
PY
}

t_only_ttl_cleanup_renders_in_every_mode() {
  each_mode only_ttl_cleanup_renders_in_every_mode <<'PY'
AGGREGATE = "rbac.kyverno.io/aggregate-to-cleanup-controller"
LABELLED = {"namespaces": "", "serviceaccounts": "",
            "roles": "rbac.authorization.k8s.io",
            "rolebindings": "rbac.authorization.k8s.io"}
def problems(docs, mode):
    out = []
    deleting = sorted(d["metadata"]["name"] for d in docs
                      if d["kind"] == "DeletingPolicy")
    if deleting:
        out.append(f"ships DeletingPolicy {deleting}")
    if "reap-after" in json.dumps(docs):
        out.append("names reap-after")
    rules = [
        r for d in docs if d["kind"] == "ClusterRole"
        and (d["metadata"].get("labels") or {}).get(AGGREGATE) == "true"
        for r in d.get("rules") or []]
    for res, group in LABELLED.items():
        verbs = {
            v for r in rules if res in (r.get("resources") or [])
            and group in (r.get("apiGroups") or []) for v in r["verbs"]}
        lack = {"list", "watch", "delete"} - verbs
        if lack:
            out.append(f"the cleanup controller lacks {sorted(lack)} on {res}")
    return out
PY
}

t_cleaner_ships_in_every_mode() {
  each_mode cleaner_ships_in_every_mode <<'PY'
SCOPE = f"{CLEANER}-namespace-scope"
def problems(docs, mode):
    names = {(d["kind"], d["metadata"]["name"]) for d in docs}
    out = []
    lack = sorted({
        ("CronJob", CLEANER), ("ServiceAccount", CLEANER),
        ("ClusterRole", CLEANER), ("ClusterRoleBinding", CLEANER),
        ("ValidatingAdmissionPolicy", SCOPE),
        ("CiliumNetworkPolicy", f"{CLEANER}-egress")} - names)
    if lack:
        out.append(f"lacks the tenant file cleaner's {lack}")
    if not any(
        d["kind"] == "ValidatingAdmissionPolicyBinding"
        and d["spec"].get("policyName") == SCOPE for d in docs):
        out.append(f"does not bind {SCOPE}")
    return out
PY
}

t_cleaner_only_gets_lists_and_patches_namespaces() {
  each_mode cleaner_only_gets_lists_and_patches_namespaces <<'PY'
def problems(docs, mode):
    role = next((
        d for d in docs if d["kind"] == "ClusterRole"
        and d["metadata"]["name"] == CLEANER), None)
    want = [{
        "apiGroups": [""], "resources": ["namespaces"],
        "verbs": ["get", "list", "patch"]}]
    if role is not None and role.get("rules") != want:
        return [f"ClusterRole {CLEANER} rules are {role.get('rules')}"]
    return []
PY
}

t_cleaner_deletes_records_only_when_on() {
  each_mode cleaner_deletes_records_only_when_on <<'PY'
def problems(docs, mode):
    cron = next((
        d for d in docs if d["kind"] == "CronJob"
        and d["metadata"]["name"] == CLEANER), None)
    if cron is None:
        return []
    spec = cron["spec"]["jobTemplate"]["spec"]["template"]["spec"]
    script = spec["containers"][0]["args"][0]
    if ("sycophant-entitlements" in script) != (mode == "on"):
        return ["the cleaner's entitlement-record delete must ship "
                "only with the setting on"]
    return []
PY
}

t_setting_off_renders_no_self_service_object
t_operator_fails_render setting_on_without_operator_fails_render
t_operator_fails_render setting_on_with_unprefixed_operator_fails_render \
  --set-string "selfService.operator=https://other.example.com/dex#operator"
t_setting_on_maps_one_constant_group
t_dex_password_id_must_be_its_name
t_setting_on_kyverno_policies_compile
t_owner_grant_fires_on_the_marker
t_only_ttl_cleanup_renders_in_every_mode
t_cleaner_ships_in_every_mode
t_cleaner_only_gets_lists_and_patches_namespaces
t_cleaner_deletes_records_only_when_on

summary

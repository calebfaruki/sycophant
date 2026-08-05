#!/usr/bin/env bash
#
# Acceptance tests for the "toolset pod collapse" leg (ADR 024 final leg).
#
# These assert the OBSERVABLE state of the repo, the rendered helm charts, the
# CLI wiring, and the proto vocabulary after the collapse. Runtime-logic ACs
# (fail-closed provider refusal; two-tier worker audience gate) live in the Rust
# acceptance tests under crates/toolset-controller/tests/ and are not duplicated
# here.
#
# Run:  bash tests/acceptance/toolset-pod-collapse.sh
# Exit: 0 iff every check passes — run as a guard against regressing the
#       collapse.
#
# Requires: helm, git, python3 (+pyyaml).

set -u

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$REPO_ROOT" || exit 2

PASS=0; FAIL=0; declare -a FAILED
ok()  { PASS=$((PASS+1)); printf '  PASS  %s\n' "$1"; }
bad() { FAIL=$((FAIL+1)); FAILED+=("$1"); printf '  FAIL  %s\n' "$1"; }
# assert <id-desc> <exit-status-of-condition>
assert()     { if [ "$2" -eq 0 ]; then ok "$1"; else bad "$1"; fi; }
assert_not() { if [ "$2" -eq 0 ]; then bad "$1"; else ok "$1"; fi; }

# absent PAT PATHSPEC... -> exit 0 when NO tracked file (this suite excluded)
# contains the fixed string PAT.
absent()    { ! git grep -qIF -- "$1" "${@:2}" ':(exclude)tests/acceptance/' 2>/dev/null; }
absent_re() { ! git grep -qIE -- "$1" "${@:2}" ':(exclude)tests/acceptance/' 2>/dev/null; }
# dep_present CARGO_TOML DEP -> crate manifest exists and lists DEP.
dep_present() { [ -f "$1" ] && grep -qE "^[[:space:]]*$2[[:space:].=\{]" "$1"; }

TENANT=/tmp/tpc_tenant.yaml
CLUSTER=/tmp/tpc_cluster.yaml
helm template t charts/sycophant-tenant \
  --set-json 'workspaces={"demo":{"storage":"1Gi"}}' >"$TENANT" 2>/dev/null \
  || { echo "FATAL: tenant chart failed to render"; exit 2; }
helm template c charts/sycophant-cluster --set policyEngine=kyverno \
  --api-versions "kyverno.io/v1/ClusterPolicy" --api-versions "kyverno.io/v1" \
  >"$CLUSTER" 2>/dev/null \
  || { echo "FATAL: cluster chart failed to render"; exit 2; }

echo "== RED-NOW checks (fail until the collapse lands) =="

# ---- Crate inventory --------------------------------------------------------
[ -d crates/toolset-controller ] && [ ! -d crates/airlock-controller ] && [ ! -d crates/hangar-controller ]
assert "crate-01 toolset-controller present; airlock/hangar-controller gone [crate-inventory]" $?
[ -d crates/toolset-proto ] && [ ! -d crates/airlock-proto ] && [ ! -d crates/hangar-proto ]
assert "crate-02 toolset-proto present; airlock/hangar-proto gone [crate-inventory]" $?
[ -d crates/toolset-runtime ] && [ ! -d crates/airlock-runtime ]
assert "crate-03 toolset-runtime present; airlock-runtime gone [crate-inventory]" $?
[ -d crates/model-provider ] && [ ! -d crates/hangar-providers ]
assert "crate-04 model-provider present; hangar-providers gone [crate-inventory]" $?
[ -d crates/prompt-toolset ]
assert "crate-05 prompt-toolset superset crate present [crate-inventory]" $?
[ ! -d crates/hangar-llm-job ]
assert "crate-06 no separate LLM-dispatch worker crate (hangar-llm-job gone) [no-llm-worker-binary]" $?

# ---- Dependency direction (structural least-privilege) ----------------------
dep_present crates/prompt-toolset/Cargo.toml model-provider \
  && ! dep_present crates/toolset-runtime/Cargo.toml model-provider \
  && ! dep_present crates/toolset-controller/Cargo.toml model-provider
assert "dep-07 model-provider linked ONLY by prompt-toolset [tool-image-no-parser / parse-not-in-controller]" $?
dep_present crates/prompt-toolset/Cargo.toml toolset-runtime
assert "dep-08 prompt-toolset inherits toolset-runtime (unified worker surface) [prompt-runs-as-toolset-worker]" $?

# ---- CRD kind: Chamber -> Toolset -------------------------------------------
absent "kind: Chamber"
assert "crd-09 no 'kind: Chamber' in tracked sources [no-Chamber-kind]" $?
[ -f charts/sycophant-cluster/crds/toolset.yaml ] && [ ! -f charts/sycophant-cluster/crds/chamber.yaml ]
assert "crd-10 CRD manifest toolset.yaml present; chamber.yaml gone [no-Chamber-CRD]" $?
grep -q "kind: Toolset" charts/sycophant-cluster/crds/toolset.yaml 2>/dev/null \
  && grep -q "name: toolsets.sycophant.md" charts/sycophant-cluster/crds/toolset.yaml 2>/dev/null
assert "crd-11 Toolset CRD declares kind Toolset / toolsets.sycophant.md [reconciled-kind-Toolset]" $?
absent 'kind = "Chamber"' crates/
assert "crd-12 no Rust CRD derives kind = \"Chamber\" [no-Chamber-kind]" $?
grep -q 'kind = "Toolset"' crates/toolset-controller/src/crd.rs 2>/dev/null
assert "crd-13 toolset-controller crd.rs derives kind = \"Toolset\" [reconciled-kind-Toolset]" $?

# ---- CLI subcommand ---------------------------------------------------------
! grep -qE "Chamber\(" cli/src/cli.rs && grep -qE "Toolset\(" cli/src/cli.rs
assert "cli-14 tenant subcommand renamed Chamber -> Toolset [syco-toolset-present/chamber-absent]" $?
[ -f cli/src/commands/toolset.rs ] && [ ! -f cli/src/commands/chamber.rs ]
assert "cli-15 command module chamber.rs -> toolset.rs [syco-toolset-present/chamber-absent]" $?

# ---- Per-toolset egress CNP selector ----------------------------------------
absent "sycophant.md/chamber"
assert "cnp-16 no tracked source keys on sycophant.md/chamber [CNP-selector]" $?
git grep -qIF -- "sycophant.md/toolset" -- cli/src/ 2>/dev/null
assert "cnp-17 CLI egress authoring keys on sycophant.md/toolset [CNP-selector]" $?

# ---- Union llm-job-egress removed -------------------------------------------
absent "llm-job-egress"
assert "union-18 no llm-job-egress policy name anywhere [union-absent]" $?
absent_re "build_llm_egress_cnp_yaml|reconcile_llm_egress_cnp"
assert "union-19 union CNP authors removed from CLI [provider/model-author-no-egress]" $?
grep -qF "llm-job" "$TENANT"
assert_not "union-20 rendered tenant chart carries no llm-job component [union-absent]" $?
[ ! -f charts/sycophant-tenant/templates/llm-job-baseline-netpol.yaml ]
assert "union-21 llm-job-baseline-netpol.yaml deleted [union-absent]" $?
absent "llm-job" charts/ ':(exclude)charts/sycophant-cluster/templates/gvisor-pod-vap.yaml'
assert "union-22 no llm-job in chart templates (VAP prose comment excepted) [union-absent]" $?

# ---- Sole jobs:create on the toolset controller -----------------------------
python3 - "$TENANT" <<'PY'
import sys, yaml
holders=[]
for d in yaml.safe_load_all(open(sys.argv[1])):
    if not d: continue
    if d.get("kind") in ("Role","ClusterRole"):
        for r in d.get("rules") or []:
            res=set(r.get("resources") or []); verbs=set(r.get("verbs") or [])
            if "jobs" in res and ("create" in verbs or "*" in verbs):
                holders.append(d["metadata"]["name"])
assert holders==["toolset-ctrl"], f"jobs:create holders={holders!r}"
PY
assert "rbac-23 exactly one Role grants jobs:create; it is toolset-ctrl [sole-jobs-create]" $?
python3 - "$TENANT" <<'PY'
import sys, yaml
bad=[]
for d in yaml.safe_load_all(open(sys.argv[1])):
    if not d: continue
    if d.get("kind") in ("Role","ServiceAccount","Deployment","Service"):
        if d["metadata"]["name"] in ("airlock-ctrl","hangar-ctrl"):
            bad.append((d["kind"], d["metadata"]["name"]))
assert not bad, f"legacy controller objects rendered: {bad!r}"
PY
assert "rbac-24 no airlock-ctrl/hangar-ctrl objects rendered [sole-jobs-create]" $?
python3 - "$TENANT" <<'PY'
import sys, yaml
docs=[d for d in yaml.safe_load_all(open(sys.argv[1])) if d]
jobs_roles={d["metadata"]["name"] for d in docs
    if d.get("kind") in ("Role","ClusterRole")
    and any("jobs" in set(r.get("resources") or []) for r in (d.get("rules") or []))}
off=[]
for d in docs:
    if d.get("kind")!="RoleBinding": continue
    if (d.get("roleRef") or {}).get("name") not in jobs_roles: continue
    for s in d.get("subjects") or []:
        n=s.get("name","")
        if "harness" in n or "relay" in n: off.append((d["metadata"]["name"], n))
assert not off, f"jobs Role bound to harness/relay SA: {off!r}"
PY
assert "rbac-25 no jobs Role bound to a harness or relay SA [harness-no-jobs / relay-no-jobs]" $?

# ---- Kyverno gVisor label guard ---------------------------------------------
python3 - "$CLUSTER" <<'PY'
import sys, yaml
rule=None
for d in yaml.safe_load_all(open(sys.argv[1])):
    if not d or d.get("kind")!="ClusterPolicy": continue
    for r in (d.get("spec") or {}).get("rules") or []:
        if r.get("name")=="restrict-tool-job-labels": rule=yaml.dump(r)
assert rule is not None, "restrict-tool-job-labels missing"
assert "toolset-ctrl" in rule, "tool-job guard does not exclude toolset-ctrl"
assert "airlock-ctrl" not in rule, "tool-job guard still excludes airlock-ctrl"
PY
assert "kyverno-26 restrict-tool-job-labels excludes toolset-ctrl, not airlock-ctrl [gVisor-select-tool-job]" $?
grep -qF "restrict-hangar-job-labels" "$CLUSTER"
assert_not "kyverno-27 restrict-hangar-job-labels policy removed [gVisor-select-tool-job]" $?

# ---- Audience remap (8 -> 6) ------------------------------------------------
grep -q '"harness.toolset.sycophant.md"' crates/shared/src/auth.rs \
  && grep -q '"toolset.toolset.sycophant.md"' crates/shared/src/auth.rs \
  && grep -q '"relay.toolset.sycophant.md"' crates/shared/src/auth.rs \
  && grep -q '"toolset.relay.sycophant.md"' crates/shared/src/auth.rs
assert "aud-28 four merged audience constants present with exact values [audience-remap / relay.toolset]" $?
absent "llm.hangar"
assert "aud-29 llm.hangar audience string gone repo-wide [llm.hangar-absent]" $?
absent_re "harness\.hangar\.sycophant|harness\.airlock\.sycophant|chamber\.airlock\.sycophant|relay\.hangar\.sycophant|hangar\.relay\.sycophant" \
  ':(exclude)charts/sycophant-cluster/templates/gvisor-pod-vap.yaml'
assert "aud-30 retired audience literals gone (VAP prose comment excepted) [audience-remap]" $?

# ---- TokenReview ClusterRole merge ------------------------------------------
python3 - "$CLUSTER" <<'PY'
import sys, yaml
names={d["metadata"]["name"] for d in yaml.safe_load_all(open(sys.argv[1]))
       if d and d.get("kind")=="ClusterRole" and "tokenreview" in d["metadata"]["name"]}
assert "cluster-toolset-tokenreview" in names, f"missing merged role; have {names!r}"
assert "cluster-airlock-tokenreview" not in names and "cluster-hangar-tokenreview" not in names, \
    f"legacy tokenreview roles remain: {names!r}"
PY
assert "token-31 one cluster-toolset-tokenreview ClusterRole; legacy ones gone [TokenReview-survives]" $?

# ---- Single controller endpoint (chart collapse) ----------------------------
python3 - "$TENANT" <<'PY'
import sys, yaml
have={(d.get("kind"), d["metadata"]["name"]) for d in yaml.safe_load_all(open(sys.argv[1])) if d}
for k in ("ServiceAccount","Deployment","Service"):
    assert (k,"toolset-ctrl") in have, f"missing {k}/toolset-ctrl"
PY
assert "chart-32 one toolset-ctrl SA/Deployment/Service rendered [chart-collapse]" $?
grep -qF "TOOLSET_CONTROLLER_ADDR" "$TENANT" \
  && ! grep -qE "HANGAR_CONTROLLER_ADDR|AIRLOCK_CONTROLLER_ADDR" "$TENANT"
assert "chart-33 harness env uses TOOLSET_CONTROLLER_ADDR only [single-address]" $?

# ---- Vocabulary collapse onto proto -----------------------------------------
absent_re "pub enum ContentBlock" crates/
assert "proto-34 no hand-written serde 'enum ContentBlock' twin remains [single-proto-vocabulary]" $?
git grep -qIF -- "FileBlock" -- crates/proto-common/proto/ 2>/dev/null \
  && git grep -qIE -- "FileBlock file =" -- crates/proto-common/proto/ 2>/dev/null
assert "proto-35 proto ContentBlock gains a FileBlock/file arm [incoming-file-representable]" $?

echo
echo "== REGRESSION GUARDS (green now; must stay green) =="
grep -qE "runtimeClassName == 'gvisor'|runtimeClassName: gvisor" charts/sycophant-cluster/templates/gvisor-pod-vap.yaml
assert "guard-G1 gVisor VAP still enforces runtimeClassName gvisor [gVisor-VAP-enforces-gvisor]" $?
grep -qF "tool-job" charts/sycophant-cluster/templates/gvisor-pod-vap.yaml
assert "guard-G2 gVisor VAP still gates on tool-job component [gVisor-VAP-gates-component]" $?
grep -qE "networkpolicies|ciliumnetworkpolicies" "$TENANT"
assert_not "guard-G3 no tenant Role grants (cilium)networkpolicies verbs [controller-no-CNP-verb]" $?

echo
echo "-------------------------------------------------------------"
printf 'RESULT: %d passed, %d failed\n' "$PASS" "$FAIL"
if [ "$FAIL" -ne 0 ]; then
  printf 'FAILED:\n'; for f in "${FAILED[@]}"; do printf '  - %s\n' "$f"; done
  exit 1
fi
exit 0

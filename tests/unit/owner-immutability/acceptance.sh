#!/usr/bin/env bash
# Offline: renders the tenant-namespace PSA-required VAP and checks that the
# sycophant.md/owner annotation and label are immutable on UPDATE.
# No cluster.
#
#   bash tests/unit/owner-immutability/acceptance.sh
set -uo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
readonly HERE
source "$HERE/../lib.sh"

helm template c "$ROOT/charts/sycophant-cluster" \
  --api-versions policies.kyverno.io/v1/ValidatingPolicy \
  --show-only templates/tenant-ns-psa-required-vap.yaml \
  >"$WORK/vap.yaml" 2>&1 || {
  bad render "$(helm_err <"$WORK/vap.yaml")"
  summary
  exit 1
}

# owner_unchecked <annotations|labels>: prints what is wrong unless exactly one
# validation names the owner <field> and its expression is the UPDATE-only
# equality of the object's and oldObject's owner values.
owner_unchecked() {
  python3 - "$WORK/vap.yaml" "$1" <<'PY'
import sys, yaml
path, field = sys.argv[1:]
docs = [d for d in yaml.safe_load_all(open(path)) if d]
vap = next(d for d in docs if d["kind"] == "ValidatingAdmissionPolicy")
named = [
    v["expression"] for v in vap["spec"]["validations"]
    if f"sycophant.md/owner {field[:-1]} " in v["message"]
]
want = ("request.operation != 'UPDATE' || "
        f"object.metadata.?{field}['sycophant.md/owner'].orValue('') == "
        f"oldObject.metadata.?{field}['sycophant.md/owner'].orValue('')")
if named != [want]:
    print(f"want [{want!r}], got {named}")
PY
}

for field in annotations labels; do
  name="owner ${field%s} immutability expression present"
  missing="$(owner_unchecked "$field" 2>&1)"
  if [ -z "$missing" ]; then
    ok "$name"
  else
    bad "$name" "$missing"
  fi
done

summary

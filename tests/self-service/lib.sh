# shellcheck shell=bash
# Shared helpers for the self-service chainsaw scripts. Each script sources it
# as `. ../lib.sh`, since chainsaw runs scripts from the test's directory.
set -euo pipefail

VALUES=$(helm get values sycophant -n sycophant-system --all -o json)
ISSUER=$(printf '%s' "$VALUES" | jq -r .idp.issuer)
readonly ISSUER
readonly ADMIN_GROUP="sycophant:tenant-admins"
OPERATOR=$(printf '%s' "$VALUES" | jq -r .selfService.operator)
# shellcheck disable=SC2034  # used by the scripts that source this file.
readonly OPERATOR

user() { printf '%s#%s' "$ISSUER" "$1"; }

# as_admin <identity> <kubectl args...>: kubectl as a tenant admin.
as_admin() {
  local who="$1"
  shift
  kubectl --as="$who" --as-group="$ADMIN_GROUP" "$@"
}

owner_hash() { printf '%s' "$1" | openssl dgst -sha256 | awk '{print $NF}' | cut -c1-16; }

hex10() { printf '%s' "$1" | openssl dgst -sha256 | awk '{print $NF}' | cut -c1-10; }

# tenant_hash <owner> <name>
tenant_hash() { printf '%s\n%s' "$1" "$2" | openssl dgst -sha256 | awk '{print $NF}' | cut -c1-10; }

# tenant_command <name> <owner> [hash]: an otherwise-valid tenant command for
# the tenant <owner> chose to call <name>. [hash] defaults to
# tenant_hash <owner> <name>; pass an existing tenant's hash to re-run it
# or to forge one.
tenant_command() {
  local hash="${3:-$(tenant_hash "$2" "$1")}"
  cat <<EOF
apiVersion: v1
kind: ConfigMap
metadata:
  name: provision-tenant-$hash
  namespace: sycophant-system
  labels:
    app.kubernetes.io/part-of: sycophant
    sycophant.md/command: "true"
  annotations:
    sycophant.md/target: "$1"
data:
  tenant: "$hash"
  capacity: 2Gi
  owner: "$2"
  owner-hash: "$(owner_hash "$2")"
EOF
}

# record <tenant hash> <owner>: the tenant's entitlement record.
record() {
  cat <<EOF
apiVersion: v1
kind: ConfigMap
metadata:
  name: $1
  namespace: sycophant-entitlements
data:
  owner: "$2"
EOF
}

# tenant_ns <namespace> <owner> [reap-after]: a tenant namespace created directly.
tenant_ns() {
  cat <<EOF
apiVersion: v1
kind: Namespace
metadata:
  name: $1
  labels:
    app.kubernetes.io/part-of: sycophant-tenant
    pod-security.kubernetes.io/enforce: restricted
    pod-security.kubernetes.io/enforce-version: latest
    pod-security.kubernetes.io/warn: restricted
    pod-security.kubernetes.io/audit: restricted
    sycophant.md/owner: "$(owner_hash "$2")"
  annotations:
    sycophant.md/owner: "$2"
EOF
  if [ -n "${3:-}" ]; then
    printf '    sycophant.md/reap-after: "%s"\n' "$3"
  fi
}

# labelled_command <verb-kind> <target> <key=value>: a well-formed in-tenant command.
labelled_command() {
  kubectl create configmap "$1-$(hex10 "$2")" --from-literal="$3" \
    --dry-run=client -o yaml \
    | kubectl label --local -f - -o yaml app.kubernetes.io/part-of=sycophant sycophant.md/command=true \
    | kubectl annotate --local -f - -o yaml "sycophant.md/target=$2"
}

# suspend_command <key=value>: the suspend-tenant singleton, which has no target.
suspend_command() {
  kubectl create configmap suspend-tenant --from-literal="$1" \
    --dry-run=client -o yaml \
    | kubectl label --local -f - -o yaml app.kubernetes.io/part-of=sycophant sycophant.md/command=true
}

# refused <want> <description> <command...>: the command must fail with an
# error containing <want>.
refused() {
  local want="$1" what="$2" err
  shift 2
  if err=$("$@" 2>&1 >/dev/null); then
    echo "$what was admitted"
    exit 1
  fi
  case "$err" in
    *"$want"*) echo "$what refused: $err" ;;
    *) echo "$what refused, but not with '$want': $err"; exit 1 ;;
  esac
}

# installer_for <command uid>: the installer Job's name for that command.
installer_for() { printf 'provision-tenant-%s' "$(printf '%s' "$1" | tr -d - | cut -c1-10)"; }

wait_job_succeeded() {
  kubectl wait --for=create --for=condition=Complete "job/$1" -n sycophant-system --timeout=280s >/dev/null \
    || { echo "installer Job $1 did not succeed"; exit 1; }
}

# wait_owner_grant <tenant-ns> <owner>: the grant arrives asynchronously.
wait_owner_grant() {
  for _ in $(seq 1 60); do
    [ "$(kubectl auth can-i create configmaps -n "$1" --as="$2")" = yes ] && return 0
    sleep 2
  done
  echo "$2 never got the owner grant in $1"
  exit 1
}

# wait_reaper_runs <since> <jq filter> <min> <tries>
wait_reaper_runs() {
  local n
  for _ in $(seq 1 "$4"); do
    n=$(kubectl get jobs -n sycophant-system -o json | jq --arg t "$1" \
      "[.items[] | select(any(.metadata.ownerReferences[]?; .kind == \"CronJob\" and .name == \"reap-tenant-files\"))
        | select(.metadata.creationTimestamp > \$t) | select($2)] | length")
    [ "$n" -ge "$3" ] && return 0
    sleep 2
  done
  return 1
}

# extra_rules <namespace> <identity>: the `auth can-i --list` rows <identity>
# holds in <namespace> beyond what an identity with no bindings holds.
extra_rules() {
  local baseline="system:serviceaccount:sycophant-system:no-bindings-probe" b w
  b=$(mktemp); w=$(mktemp)
  kubectl auth can-i --list -n "$1" --as="$baseline" --no-headers | sort > "$b"
  kubectl auth can-i --list -n "$1" --as="$2" --no-headers | sort > "$w"
  comm -13 "$b" "$w" | tr -s ' ' | sed 's/ $//'
  rm -f "$b" "$w"
}

#!/usr/bin/env bash
# Store-agnostic read-contract probe for agent-token-reads-only-its-folder.
# Talks to the store over S3 and STS only (aws CLI over a test port-forward);
# the seed and clean subcommands write the fixture objects store-side, the way
# an operator upload would, through the filer inside the store pod.
#
#   contract.sh <seed|clean|all>
#
# `all` opens one port-forward, runs the positive-control STS exchange once
# and checks every case against the credentials it returns.
#
# Env: TENANT_NS (holds ag-foo, ag-foobar and the probe pod running as ag-foo).
set -euo pipefail

: "${TENANT_NS:?}"
readonly BUCKET="sycophant-instructions"
readonly FILER="http://localhost:8888/buckets/${BUCKET}"
readonly READER="arn:aws:iam::role/agent-instructions-read"
readonly PROBE_POD="ag-foo-probe"

sub() { printf 'system:serviceaccount:%s:%s' "$1" "$2"; }
FOO="$(sub "$TENANT_NS" ag-foo)"
FOOBAR="$(sub "$TENANT_NS" ag-foobar)"
readonly FOO FOOBAR

# store_put <sub> <relpath> <content>: one object, written store-side.
store_put() {
  printf '%s' "$3" | kubectl exec -i -n sycophant-system deploy/seaweedfs -- \
    curl -fsS -g -o /dev/null -T - "${FILER}/$1/$2"
}

# store_rm <sub>: the folder, removed store-side.
store_rm() {
  kubectl exec -n sycophant-system deploy/seaweedfs -- \
    curl -fsS -g -o /dev/null -X DELETE "${FILER}/$1/?recursive=true"
}

# open_store <workdir>: port-forward the store's S3/STS port to a free local
# port; sets ENDPOINT. The EXIT trap kills the forward and removes <workdir>.
open_store() {
  local workdir="$1" port pf_pid
  port="$(python3 -c 'import socket; s = socket.socket(); s.bind(("127.0.0.1", 0)); print(s.getsockname()[1])')"
  kubectl port-forward -n sycophant-system svc/seaweedfs "${port}:8333" > "$workdir/pf.log" 2>&1 &
  pf_pid=$!
  # shellcheck disable=SC2064  # bind the pid and dir now
  trap "kill $pf_pid 2>/dev/null || true; wait $pf_pid 2>/dev/null || true; rm -rf '$workdir'" EXIT
  ENDPOINT="http://127.0.0.1:${port}"
  local _
  for _ in $(seq 1 30); do
    if curl -s -o /dev/null "$ENDPOINT"; then
      return 0
    fi
    sleep 1
  done
  echo "the store port-forward never answered" >&2
  cat "$workdir/pf.log" >&2
  exit 1
}

# aws with no ambient credential or profile.
aws_clean() {
  env -u AWS_ACCESS_KEY_ID -u AWS_SECRET_ACCESS_KEY -u AWS_SESSION_TOKEN -u AWS_PROFILE \
    AWS_CONFIG_FILE=/dev/null AWS_SHARED_CREDENTIALS_FILE=/dev/null AWS_REGION=us-east-1 AWS_PAGER= AWS_MAX_ATTEMPTS=2 \
    aws "$@"
}

# pod_token <audience-dir> <out>: the probe pod's own projected token.
pod_token() {
  kubectl exec -n "$TENANT_NS" "$PROBE_POD" -- cat "/var/run/secrets/$1/token" > "$2"
}

# assume <token-file> <creds-out> <err-out>: STS AssumeRoleWithWebIdentity
# naming the reader role. Non-zero when the store refuses the exchange.
assume() {
  aws_clean sts assume-role-with-web-identity --role-arn "$READER" \
    --role-session-name read-contract --web-identity-token "$(cat "$1")" \
    --endpoint-url "$ENDPOINT" --output json > "$2" 2> "$3"
}

# s3 <creds-file> <s3api args...>: an S3 call with the STS credentials.
s3() {
  local creds="$1"
  shift
  local ak sk st
  ak="$(python3 -c 'import json, sys; print(json.load(open(sys.argv[1]))["Credentials"]["AccessKeyId"])' "$creds")"
  sk="$(python3 -c 'import json, sys; print(json.load(open(sys.argv[1]))["Credentials"]["SecretAccessKey"])' "$creds")"
  st="$(python3 -c 'import json, sys; print(json.load(open(sys.argv[1]))["Credentials"]["SessionToken"])' "$creds")"
  env -u AWS_PROFILE AWS_ACCESS_KEY_ID="$ak" AWS_SECRET_ACCESS_KEY="$sk" AWS_SESSION_TOKEN="$st" \
    AWS_CONFIG_FILE=/dev/null AWS_SHARED_CREDENTIALS_FILE=/dev/null AWS_REGION=us-east-1 AWS_PAGER= AWS_MAX_ATTEMPTS=2 \
    aws --endpoint-url "$ENDPOINT" s3api "$@"
}

# reader_creds <workdir>: the positive control every case rests on: the probe
# pod's sycophant-objectstore token, exchanged for reader credentials.
reader_creds() {
  local workdir="$1"
  pod_token objectstore "$workdir/store.jwt"
  if ! assume "$workdir/store.jwt" "$workdir/creds.json" "$workdir/sts.err"; then
    echo "positive control: STS refused the agent pod's sycophant-objectstore token for the reader role: $(cat "$workdir/sts.err")" >&2
    exit 1
  fi
}

# refused <what> <err-file>: the call failed with an access refusal the store
# returned (the aws CLI's "An error occurred (" service-error text), not a
# missing object or a transport error.
refused() {
  if ! grep -qF 'An error occurred (' "$2" \
     || ! grep -qiE 'AccessDenied|Forbidden|\(403\)' "$2"; then
    echo "$1 failed, but not as an access refusal from the store: $(cat "$2")" >&2
    exit 1
  fi
}

# sts_refused <what> <err-file>: STS itself refused the exchange (the aws
# CLI's "An error occurred (" service-error text for the
# AssumeRoleWithWebIdentity operation), not a transport or local failure.
sts_refused() {
  if ! grep -qF 'An error occurred (' "$2" \
     || ! grep -qF 'AssumeRoleWithWebIdentity operation' "$2"; then
    echo "$1 failed, but not as a refusal from STS: $(cat "$2")" >&2
    exit 1
  fi
}

# list_keys <creds> <out> <err> <prefix>: ListObjectsV2 keys, one per line.
list_keys() {
  local creds="$1" out="$2" err="$3" prefix="$4"
  local raw
  raw="$(s3 "$creds" list-objects-v2 --bucket "$BUCKET" --prefix "$prefix" --output json 2> "$err")" || return 1
  printf '%s' "$raw" | python3 -c 'import json, sys
d = json.loads(sys.stdin.read() or "{}")
print("\n".join(sorted(o["Key"] for o in d.get("Contents") or [])))' > "$out"
}

case_seed() {
  store_put "$FOO" AGENTS.md "foo-agents"
  store_put "$FOO" agents/poet.md "foo-poet"
  store_put "$FOOBAR" AGENTS.md "foobar-agents"
}

case_clean() {
  local s
  for s in "$FOO" "$FOOBAR"; do
    store_rm "$s" || true
  done
}

# The credentials list and read every object in the agent's folder.
case_own_folder() {
  local w="$1"
  if ! list_keys "$w/creds.json" "$w/keys" "$w/list.err" "$FOO/"; then
    echo "listing the agent's own folder ${FOO}/ was refused: $(cat "$w/list.err")" >&2
    exit 1
  fi
  local want
  want="$(printf '%s\n%s' "$FOO/AGENTS.md" "$FOO/agents/poet.md")"
  if [ "$(cat "$w/keys")" != "$want" ]; then
    echo "own-folder listing returned [$(tr '\n' ' ' < "$w/keys")], want [${want//$'\n'/ }]" >&2
    exit 1
  fi
  local key body
  for key in AGENTS.md:foo-agents agents/poet.md:foo-poet; do
    if ! s3 "$w/creds.json" get-object --bucket "$BUCKET" --key "$FOO/${key%%:*}" "$w/obj" > /dev/null 2> "$w/get.err"; then
      echo "reading own object ${FOO}/${key%%:*} was refused: $(cat "$w/get.err")" >&2
      exit 1
    fi
    body="$(cat "$w/obj")"
    if [ "$body" != "${key#*:}" ]; then
      echo "own object ${FOO}/${key%%:*} read back '${body}', want '${key#*:}'" >&2
      exit 1
    fi
  done
  echo "the agent token lists and reads its own folder"
}

# A prefix-sharing sibling agent's object is refused.
case_other_folders() {
  local w="$1" key="$FOOBAR/AGENTS.md"
  if s3 "$w/creds.json" get-object --bucket "$BUCKET" --key "$key" "$w/obj" > /dev/null 2> "$w/get.err"; then
    echo "the agent token read ${key}, another agent's object" >&2
    exit 1
  fi
  refused "GetObject ${key}" "$w/get.err"
  echo "the prefix-sharing sibling's object is refused"
}

# Put and delete are refused, in the agent's own folder too.
case_no_writes() {
  local w="$1"
  printf 'injected' > "$w/inject"
  if s3 "$w/creds.json" put-object --bucket "$BUCKET" --key "$FOO/injected.md" --body "$w/inject" > /dev/null 2> "$w/put.err"; then
    echo "the agent token wrote ${FOO}/injected.md" >&2
    exit 1
  fi
  refused "PutObject ${FOO}/injected.md" "$w/put.err"
  if s3 "$w/creds.json" delete-object --bucket "$BUCKET" --key "$FOO/AGENTS.md" > /dev/null 2> "$w/del.err"; then
    echo "the agent token's DeleteObject on ${FOO}/AGENTS.md was accepted" >&2
    exit 1
  fi
  refused "DeleteObject ${FOO}/AGENTS.md" "$w/del.err"
  echo "put and delete are refused"
}

# Listing the agent prefix without its trailing slash (which also
# prefixes the sibling ag-foobar) is refused or returns no other folder's keys.
case_listings() {
  local w="$1" leaked
  if ! list_keys "$w/creds.json" "$w/keys" "$w/list.err" "$FOO"; then
    refused "listing prefix ${FOO}" "$w/list.err"
    echo "the no-slash listing is refused"
    return 0
  fi
  leaked="$(grep -v -F "${FOO}/" "$w/keys" || true)"
  if [ -n "$leaked" ]; then
    echo "listing no-slash returned other folders' keys: ${leaked//$'\n'/ }" >&2
    exit 1
  fi
  echo "the no-slash listing leaks no other folder's keys"
}

# The same pod's relay-audience token, naming the reader role, is refused
# at STS.
case_wrong_audience() {
  local w="$1"
  pod_token relay "$w/relay.jwt"
  if assume "$w/relay.jwt" "$w/relay-creds.json" "$w/relay-sts.err"; then
    echo "STS exchanged a harness.relay.sycophant.md-audience token for the reader role" >&2
    exit 1
  fi
  sts_refused "AssumeRoleWithWebIdentity with the relay-audience token" "$w/relay-sts.err"
  echo "a token with another audience is refused at STS: $(head -c 200 "$w/relay-sts.err")"
}

case_all() {
  local w
  w="$(mktemp -d)"
  open_store "$w"
  reader_creds "$w"
  case_own_folder "$w"
  case_other_folders "$w"
  case_no_writes "$w"
  case_listings "$w"
  case_wrong_audience "$w"
}

main() {
  local cmd="${1:?usage: contract.sh <seed|clean|all>}"
  case "$cmd" in
    seed) case_seed ;;
    clean) case_clean ;;
    all) case_all ;;
    *) echo "unknown case: $cmd" >&2; exit 2 ;;
  esac
}

main "$@"

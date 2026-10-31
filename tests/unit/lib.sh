# shellcheck shell=bash
# tests/unit/lib.sh — shared boilerplate for unit acceptance scripts.
# The caller must set HERE before sourcing.
# shellcheck disable=SC2154  # HERE is set by the caller.
# shellcheck disable=SC2034  # ROOT is used by the scripts that source this file.
ROOT="$(cd "$HERE/../../.." && pwd)"
readonly ROOT
WORK="$(mktemp -d)"
readonly WORK
trap 'rm -rf "$WORK"' EXIT

pass=0
fail=0
ok()  { pass=$((pass+1)); printf 'ok   %s\n' "$1"; }
bad() { fail=$((fail+1)); printf 'FAIL %s\n    %s\n' "$1" "$2"; }

summary() {
  printf '\n%d passed, %d failed\n' "$pass" "$fail"
  [ "$fail" -eq 0 ]
}

results() {
  [ -f "$WORK/$1.json" ] || kyverno test "$WORK/$1" -o json --detailed-results --remove-color 2>/dev/null \
    | sed -n '/^\[/,/^\]/p' | jq -s 'add // []' > "$WORK/$1.json"
  cat "$WORK/$1.json"
}

expect() {
  local dir="$1" resource="$2" token="$3" out row
  out="$(results "$dir")"
  row="$(jq -c --arg r "$resource" '[.[] | select(.RESOURCE | endswith("/" + $r))][0] // empty' <<<"$out")"
  if [ -z "$row" ]; then
    bad "$dir/$resource" "no result reported"
  elif [ "$(jq -r .RESULT <<<"$row")" != "Pass" ]; then
    bad "$dir/$resource" "$(jq -r '.REASON + ": " + (.Message // "")' <<<"$row")"
  elif [ -n "$token" ] && ! jq -e --arg t "$token" '(.Message // "") | contains($t)' <<<"$row" >/dev/null; then
    bad "$dir/$resource" "rejection does not name '$token': $(jq -r '.Message // ""' <<<"$row")"
  else
    ok "$dir/$resource"
  fi
}

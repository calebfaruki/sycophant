#!/usr/bin/env bash
# Zero-match gate: the retired Enrollment kind, RPC, and code-minting path must
# leave no trace, and the two relocations that would otherwise be checked by eye
# must hold.
#
# Each gate is a zero-match assertion. A manual sweep is not a test: the failure
# mode is one forgotten file, and a human reading hundreds of matches finds all
# but one of them.
#
# Run:
#   scripts/retired-mechanisms-sweep-gate.sh
#
# Exits non-zero and prints every surviving match, per gate.
#
# `grep -rn`, never `git grep`: new files are untracked and invisible to
# `git grep`, and this sweep exists to catch exactly the file nobody remembered.
#
# Excluded everywhere: build output (target/), git internals, agent worktrees
# under .claude/, cargo-mutants scratch (mutants.out/), and the e2e runtime data
# dir (tmp/). None is repo source.
set -uo pipefail

REPO_ROOT="$(git rev-parse --show-toplevel)"
cd "$REPO_ROOT"

EXCLUDES=(
  --exclude-dir=target
  --exclude-dir=.git
  --exclude-dir=.claude
  --exclude-dir=mutants.out
  --exclude-dir=tmp
  --exclude-dir=build
  --exclude-dir=.dart_tool
  --exclude=*.patch
  # This script names every string it forbids, so it never matches itself.
  --exclude=retired-mechanisms-sweep-gate.sh
)

# A guard test has to name what it forbids, so tests/ is out of scope for the
# gates whose forbidden string appears in a guard's own assertions. What is
# being swept is the framework, not its guards. charts/ coverage is doubled by
# the chainsaw test at
# tests/integration/sa-permission-bounds/tenant-roles-omit-retired-enrollment-crd.
#
# Applied per gate, never globally. The RedeemEnrollment gate is broad on
# purpose, client and test dirs included. The code-minting gate must still catch
# a signing helper resurrected inside a test target.
GUARD_EXCLUDES=(
  --exclude-dir=tests
)

failures=0

# gate <name> <expectation> -- <grep args...>
gate() {
  local name="$1"; shift
  local why="$1"; shift
  shift # the literal --
  local hits
  hits="$(grep -rn "${EXCLUDES[@]}" "$@" . 2>/dev/null || true)"
  if [ -n "$hits" ]; then
    failures=$((failures + 1))
    printf '\n=== FAIL: %s ===\n%s\n' "$name" "$why"
    printf '%s\n' "$hits" | sed 's/^/    /'
    printf '  (%s matches)\n' "$(printf '%s\n' "$hits" | grep -c .)"
  else
    printf 'ok  %s\n' "$name"
  fi
}

# The kind, its plural, and its short name.
gate "the Enrollment kind is gone" \
  "No CRD manifest, no group-qualified plural, no 'enr' short name." \
  -- "${GUARD_EXCLUDES[@]}" -e 'enrollments\.sycophant\.md' -e 'kind: Enrollment'

# The RPC is renamed to RedeemCode, client included, because the proto
# changed. This is the one enrollment-shaped identifier the client may NOT keep.
gate "RedeemEnrollment is gone, client included" \
  "The relay accepts a registered public key only on RedeemCode (and, from phase 3, RegisterAdapterKey)." \
  -- -e RedeemEnrollment

# No code minting, signing, or expiry verification survives. The relay mints
# nothing: the code is the operator-verified row's identity, written by the
# operator like any other identity.
gate "no code minting, signing, or expiry path survives" \
  "crates/relay-controller/src/enrollment.rs and its jsonwebtoken dependency go in full." \
  -- -e sign_enrollment_code -e verify_enrollment_code -e jsonwebtoken

# tsnetBridge values, templates, and RBAC gates all belong to the app adapter
# now; five gates on `.Values.tsnetBridge.enabled` had to move at once, and
# this is what proves none was missed.
#
# tests/ is excluded here because the guard test that asserts the relay sheds
# tsnet names these strings in its own assertions
# (adapter-pod-shape/relay-renders-no-tailscale-container). The --include
# filters keep charts/, crates/, and examples/ coverage intact.
gate "tsnetBridge is gone from charts and crates" \
  "The tsnet terminus lives on the app adapter Deployment, configured under .Values.channels." \
  -- --include=*.yaml --include=*.json --include=*.rs --include=*.tpl "${GUARD_EXCLUDES[@]}" -e 'tsnetBridge' -e 'tsnet-bridge'

# The app port leaves loopback; nothing may still assume the
# same-pod sidecar shape, including the e2e port-forward target and the
# client-facing addresses the script prints.
gate "the loopback app-port literal is gone" \
  "The relay's app port binds an in-cluster address, admitted only from the app adapter pod." \
  -- -e '127\.0\.0\.1:9091'

printf '\n'
if [ "$failures" -ne 0 ]; then
  printf '%d gate(s) failed.\n' "$failures"
  exit 1
fi
printf 'all gates clean.\n'

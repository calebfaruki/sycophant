#!/usr/bin/env bash
# Zero-match gate: a toolset entry is runtime shape only -- image, keepalive,
# env. Its credential and egress axes, and the per-toolset egress policy that
# rendered from them, must leave no trace. Credentials and destinations come
# from the binding workspace's grant menu instead.
#
# Each gate is a zero-match assertion. A manual sweep is not a test: the failure
# mode is one forgotten file, and raw manifests in shell heredocs and chainsaw
# fixtures are invisible to both `cargo` and `helm`.
#
# Run:
#   scripts/toolset-axes-sweep-gate.sh
#
# Exits non-zero and prints every surviving match, per gate.
#
# `grep -rn`, never `git grep`: new files are untracked and invisible to
# `git grep`, and this sweep exists to catch exactly the file nobody remembered.
#
# Excluded everywhere: build output (target/), git internals, agent worktrees
# under .claude/, cargo-mutants scratch (mutants.out/), the e2e runtime data dir
# (tmp/), and the vendored Kyverno CRDs, whose own schemas carry an unrelated
# `secrets:` key.
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
  --exclude-dir=kyverno-crds
  --exclude=*.patch
  # This script names every string it forbids, so it never matches itself.
  --exclude=toolset-axes-sweep-gate.sh
)

# A guard has to name what it forbids, so the two buckets that assert the
# retired keys are rejected are out of scope for the gates whose forbidden
# string appears in their own assertions. What is being swept is the framework,
# not its guards.
GUARD_EXCLUDES=(
  --exclude-dir=toolset-grants
  --exclude=toolset_entry_axes.rs
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

# The wire shape the axis had: a `secrets:` list whose items pair a Secret name
# with an `env` or `file` target. A prompt profile's singular `secret:` and a
# grant's `secret:` are different keys, are not list items, and survive.
gate "the toolset entry secrets list shape is gone" \
  "Chart values and their comment block, the ConfigMap template, the e2e values fixture, example values, and the operator docs." \
  -- "${GUARD_EXCLUDES[@]}" -E -e '^ *- secret: \S' -e '^ *secrets:$'

# The entry's two axes in the values schema, which is the gate an operator hits
# first. The prompt profile's own `secret` lives elsewhere in the same file and
# survives.
gate "the toolset entry axes are gone from the values schema" \
  "values.schema.json must declare no secrets or egress under toolsets.*." \
  -- --include=values.schema.json -e '"secrets"'

# The controller reading either axis off an entry, and the env-var delivery path
# the credential axis owned. Every credential is a file now.
gate "no controller code reads an entry axis or delivers a credential as env" \
  "ToolsetEntry.secrets, ToolsetEntry.egress, SecretTarget::Env, secret_key_ref_env, and RawSecretMapping." \
  -- --include=*.rs "${GUARD_EXCLUDES[@]}" -e 'entry\.secrets' -e 'entry\.egress' -e 'SecretTarget::Env' -e 'secret_key_ref_env' -e 'RawSecretMapping'

# The per-toolset egress policy that rendered from the axis. The prompt-profile
# arm of the same template survives.
gate "no CiliumNetworkPolicy renders from a toolset entry" \
  "prompt-egress-netpol.yaml iterates prompt profiles only." \
  -- --include=*.tpl --include=*.yaml "${GUARD_EXCLUDES[@]}" -e '\$entry\.egress' -e 'range \$toolset, \$entry := \.Values\.toolsets'

printf '\n'
if [ "$failures" -ne 0 ]; then
  printf '%d gate(s) failed.\n' "$failures"
  exit 1
fi
printf 'all gates clean.\n'

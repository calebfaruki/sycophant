#!/usr/bin/env bash
# Zero-match gate: the prompt request path names no response body parameter.
# The request path passes `None` for params and no proto carries a params
# field, so logprobs, logit bias, and the SSE keepalive override have no wire
# source. That absence is the whole control -- no endpoint rejects a body, and
# wiring a params map opens every excluded key at once. This gate holds the
# absence closed: it fails the moment the request path builds a params map or
# names one of the forbidden keys.
#
# Each gate is a zero-match assertion. A manual read is not a test: the failure
# mode is one line added to main.rs, and the diff that opens the gap is small.
#
# Run:
#   scripts/prompt-params-sweep-gate.sh
#
# Exits non-zero and prints every surviving match, per gate.
#
# `grep -rn`, never `git grep`: new files are untracked and invisible to
# `git grep`, and this sweep exists to catch exactly the file nobody remembered.
#
# Scope is crates/prompt-toolset/src, the request path. crates/model-provider is
# the generic provider layer -- where a managed body would be built for a caller
# that supplied params -- and stays out of scope: this crate never supplies them.
#
# `max_tokens` gets no gate: main.rs already carries it as a response
# stop-reason string, so a bare match false-positives, and naming it on a
# request requires a params map the first gate already forbids. `from_str` gets
# no gate: config.rs uses it for the format discriminator, and the named-key
# gates cover what it could otherwise carry.
set -uo pipefail

REPO_ROOT="$(git rev-parse --show-toplevel)"
cd "$REPO_ROOT"

# Every gate reads only the request path. The provider layer is deliberately
# not swept here.
SCOPE="crates/prompt-toolset/src"

EXCLUDES=(
  --exclude-dir=target
  --exclude-dir=.git
  --exclude-dir=.claude
  --exclude-dir=mutants.out
  --exclude-dir=tmp
)

# A guard has to name what it forbids. This sweep's mechanism is absence, not
# rejection, so no in-scope fixture asserts these keys are rejected and there is
# no other file to exempt. The script itself is the only file that names every
# forbidden string, so it excludes itself the way toolset-axes-sweep-gate.sh
# does, and nothing more.
GUARD_EXCLUDES=(
  --exclude=prompt-params-sweep-gate.sh
)

failures=0

# gate <name> <expectation> -- <grep args...>
gate() {
  local name="$1"; shift
  local why="$1"; shift
  shift # the literal --
  local hits
  hits="$(grep -rn "${EXCLUDES[@]}" "$@" "$SCOPE" 2>/dev/null || true)"
  if [ -n "$hits" ]; then
    failures=$((failures + 1))
    printf '\n=== FAIL: %s ===\n%s\n' "$name" "$why"
    printf '%s\n' "$hits" | sed 's/^/    /'
    printf '  (%s matches)\n' "$(printf '%s\n' "$hits" | grep -c .)"
  else
    printf 'ok  %s\n' "$name"
  fi
}

# A params map is the single wire that opens every excluded key. The request
# path passes a literal `None` instead and builds no map at all.
gate "the request path constructs no params map" \
  "main.rs must pass no params to the provider: no serde_json map is built." \
  -- "${GUARD_EXCLUDES[@]}" -e 'Map::new' -e 'json!' -e 'Value::Object'

# The excluded body parameters, named directly. None can reach the wire without
# the map the first gate forbids, and none is named here regardless.
gate "no excluded body parameter is named" \
  "logprobs, top_logprobs, logit_bias, and n_probs are out of the request path." \
  -- "${GUARD_EXCLUDES[@]}" -e 'logprobs' -e 'top_logprobs' -e 'logit_bias' -e 'n_probs'

# The per-request SSE keepalive override. Disabling it would let a slow prefill
# stream die at the L7 proxy's idle timeout; the interval is pinned on the
# engine command line and no request may override it.
gate "no SSE keepalive override is named" \
  "sse_ping_interval is set on the engine command line, never per request." \
  -- "${GUARD_EXCLUDES[@]}" -e 'sse_ping_interval'

printf '\n'
if [ "$failures" -ne 0 ]; then
  printf '%d gate(s) failed.\n' "$failures"
  exit 1
fi
printf 'all gates clean.\n'

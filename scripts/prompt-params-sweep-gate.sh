#!/usr/bin/env bash
# Zero-match gate: the prompt request path can name no body parameter.
#
# The model server rejects nothing and no layer between the prompt job and it
# inspects a request body. Cilium cannot help either -- its HTTP rules match
# method, path, host and headers, and the proxy wire protocol carries header
# matchers only. So the parameters that stay outside the permitted surface stay
# outside it by ABSENCE: the prompt job passes no params map, no protobuf
# carries one, and nothing downstream can invent one.
#
# That absence is latent, not live. Wiring a params map through opens every
# excluded parameter at once, and the same edit turns the server's output
# default into something a request can raise and its stream keepalive into
# something a request can disable. This gate makes wiring one through a
# deliberate change rather than an incidental one.
#
# The generic provider layer under crates/model-provider names these keys
# legitimately -- it speaks whole vendor wire formats. What is being swept is
# the prompt request path, so every gate is scoped to crates/prompt-toolset/src.
#
# Run:
#   scripts/prompt-params-sweep-gate.sh
#
# Exits non-zero and prints every surviving match, per gate.
#
# `grep -rn`, never `git grep`: new files are untracked and invisible to
# `git grep`, and this sweep exists to catch exactly the file nobody remembered.
set -uo pipefail

REPO_ROOT="$(git rev-parse --show-toplevel)"
cd "$REPO_ROOT"

SCOPE="crates/prompt-toolset/src"

EXCLUDES=(
  --exclude-dir=target
  --exclude-dir=.git
  --exclude-dir=.claude
  --exclude-dir=mutants.out
  --exclude=*.patch
  # This script names every string it forbids, so it never matches itself.
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

# The construction itself. The provider clients clone a params map wholesale and
# manage only model, messages, tools and stream, so any map that reaches them
# passes every other key straight through to the server.
#
# This is also what holds the output default. The engine treats the server's
# `-n` as the fallback for a request that names no value, never as a ceiling, so
# a request able to name `max_tokens` overrides it. `max_tokens` needs no gate
# of its own: naming it requires a params map, and the response stop-reason
# string of the same name would make a bare match a false positive.
gate "the prompt request path constructs no params map" \
  "crates/prompt-toolset/src must build no JSON object to hand the provider client." \
  -- --include=*.rs -e 'Map::new' -e 'json!' -e 'Value::Object'

# The decode-time parameters outside the permitted surface. Grammar and
# schema-constrained decoding are inside it and are deliberately absent here.
gate "the prompt request path names no excluded decode parameter" \
  "logprobs, top_logprobs, logit_bias and n_probs are outside the text-only surface." \
  -- --include=*.rs -e 'logprobs' -e 'top_logprobs' -e 'logit_bias' -e 'n_probs'

# The stream keepalive is what carries an SSE response through a prefill long
# enough to trip the L7 proxy's idle timeout. It is pinned on the server's
# command line, and a request able to name it per-stream can turn it off for
# its own stream and be cut mid-prefill.
gate "the prompt request path names no stream keepalive override" \
  "sse_ping_interval is a server-side flag, never a request field." \
  -- --include=*.rs -e 'sse_ping_interval'

printf '\n'
if [ "$failures" -ne 0 ]; then
  printf '%d gate(s) failed.\n' "$failures"
  exit 1
fi
printf 'all gates clean.\n'

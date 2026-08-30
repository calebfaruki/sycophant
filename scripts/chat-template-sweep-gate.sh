#!/usr/bin/env bash
# Zero-match gate: the framework carries chat-template plumbing but no template
# content. An operator supplies a corrected template through an uncommitted
# values overlay; the chart renders only the ConfigMap, volume, mount, and arg
# that carry it. That split is the whole control -- no model-specific text ships
# and the framework never inspects a template. This gate holds the absence
# closed: it fails the moment a `.jinja` file, a Jinja control marker, or a
# ChatML template token lands under `charts/` or `examples/`.
#
# Each gate is a zero-match assertion. A manual read is not a test: the failure
# mode is one file committed under the chart, and the diff that opens the gap is
# small.
#
# Run:
#   scripts/chat-template-sweep-gate.sh
#
# Exits non-zero and prints every surviving match, per gate.
#
# `grep -rn`, never `git grep`: new files are untracked and invisible to
# `git grep`, and this sweep exists to catch exactly the file nobody remembered.
#
# Scope is charts/ and examples/. The e2e overlay under docs/ is the sole
# disclosed fixture that carries a template and is deliberately out of scope.
#
# Extension checks key on `\.jinja` WITH the dot: the engine's `--jinja` flag in
# templates/inference.yaml renders the model's own embedded template and is not a
# committed template file, so a bare `jinja` match would false-positive on it.
set -uo pipefail

REPO_ROOT="$(git rev-parse --show-toplevel)"
cd "$REPO_ROOT"

# The framework halves that may only carry plumbing, never content. docs/ is
# excluded by omission: the e2e overlay lives there by design.
SCOPE=(charts examples)

EXCLUDES=(
  --exclude-dir=target
  --exclude-dir=.git
  --exclude-dir=.claude
  --exclude-dir=mutants.out
  --exclude-dir=tmp
)

# This script is the only file in scope that names every forbidden literal, so
# it excludes itself the way prompt-params-sweep-gate.sh does, and nothing more.
GUARD_EXCLUDES=(
  --exclude=chat-template-sweep-gate.sh
)

failures=0

# gate <name> <expectation> -- <grep args...>
gate() {
  local name="$1"; shift
  local why="$1"; shift
  shift # the literal --
  local hits
  # -I skips binary files: vendored chart archives (*.tgz) under charts/ carry
  # the byte pair by coincidence and cannot hold a readable template body.
  hits="$(grep -rnI "${EXCLUDES[@]}" "$@" "${SCOPE[@]}" 2>/dev/null || true)"
  if [ -n "$hits" ]; then
    failures=$((failures + 1))
    printf '\n=== FAIL: %s ===\n%s\n' "$name" "$why"
    printf '%s\n' "$hits" | sed 's/^/    /'
    printf '  (%s matches)\n' "$(printf '%s\n' "$hits" | grep -c .)"
  else
    printf 'ok  %s\n' "$name"
  fi
}

# A committed `.jinja` file is a template file by extension. find, not grep,
# because the match is the path itself, not a line in it.
jinja_files="$(find "${SCOPE[@]}" -name '*.jinja' 2>/dev/null || true)"
if [ -n "$jinja_files" ]; then
  failures=$((failures + 1))
  printf '\n=== FAIL: %s ===\n%s\n' "no committed .jinja file under the framework" \
    "charts/ and examples/ carry plumbing only; a .jinja file is model-specific template content."
  printf '%s\n' "$jinja_files" | sed 's/^/    /'
else
  printf 'ok  %s\n' "no committed .jinja file under the framework"
fi

# Jinja control markers. Helm renders with `{{ }}` and never `{% %}`, so this is
# a clean discriminator for a template body pasted into a chart or example.
gate "no Jinja control marker under the framework" \
  "charts/ and examples/ use Helm's {{ }}; a {% %} block is chat-template content." \
  -- "${GUARD_EXCLUDES[@]}" -F -e '{%'

# ChatML role tokens. A corrected instruct-model template opens and closes turns
# with these; their presence is embedded template content by construction.
gate "no ChatML template token under the framework" \
  "<|im_start|> / <|im_end|> are chat-template content, not chart plumbing." \
  -- "${GUARD_EXCLUDES[@]}" -F -e '<|im_start|>' -e '<|im_end|>'

printf '\n'
if [ "$failures" -ne 0 ]; then
  printf '%d gate(s) failed.\n' "$failures"
  exit 1
fi
printf 'all gates clean.\n'

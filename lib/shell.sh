# shellcheck shell=bash
# lib/shell.sh — shared UI helpers and error handling for sycophant shell
# scripts. Source once; a second source is a no-op.
# The caller must `set -euo pipefail` BEFORE sourcing this file.
[ -n "${_SYCOPHANT_SHELL_SH:-}" ] && return 0
_SYCOPHANT_SHELL_SH=1

# ---- error handling (strict-tier scripts) ----
set -o errtrace
export PS4='+ ${BASH_SOURCE}:${LINENO} ${FUNCNAME[0]:-main}: '
on_error() {
  local line=$1 cmd="$2"
  echo "FAIL at line $line: $cmd" >&2
  echo "call stack:" >&2
  for ((i = ${#FUNCNAME[@]} - 1; i >= 0; i--)); do
    printf '  %s() at %s:%s\n' "${FUNCNAME[$i]}" "${BASH_SOURCE[$i]:-}" \
      "${BASH_LINENO[$i - 1]:-}" >&2
  done
}
trap 'on_error $LINENO "$BASH_COMMAND"' ERR

# ---- ui helpers ----
_step_start=0
# step_elapsed: how long the step in progress took, once one has started.
step_elapsed() {
  if [ "$_step_start" -gt 0 ]; then
    printf '\033[1;34m    ⏱  previous step took %ds\033[0m\n' \
      "$((SECONDS - _step_start))"
  fi
}
step() {
  step_elapsed
  _step_start=$SECONDS
  printf '\n\033[1;36m==> %s\033[0m\n' "$*"
}
ok() { printf '\033[1;32m ✓\033[0m %s\n' "$*"; }
warn() { printf '\033[1;33m ⚠\033[0m %s\n' "$*" >&2; }

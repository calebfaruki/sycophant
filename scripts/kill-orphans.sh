#!/usr/bin/env bash
# Kill leftover sycophant test processes from prior e2e / quickstart runs.
# Idempotent. Safe to run manually after a session that exited uncleanly.
#
# Targets:
#   - Leaked cargo test binaries (`target/{debug,release}/deps/*`).
#   - kubectl port-forward processes against sycophant components.
#   - Bash `while true` respawn loops wrapping those port-forwards.
#
# Always exits 0 — orphan absence is not an error.
set -uo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SELF_PID=$$

# Order matters: kill wrappers BEFORE kubectl children so the wrapper
# doesn't respawn the child mid-sweep.
patterns=(
  "while true.*kubectl.*port-forward.*(tightbeam-ctrl|headscale|airlock-ctrl|mainframe-ctrl|e2e-test)"
  "kubectl.*port-forward.*(tightbeam-ctrl|headscale|airlock-ctrl|mainframe-ctrl|e2e-test)"
  "${REPO_ROOT}/target/(debug|release)/deps/"
)

found=0
sweep() {
  local sig=$1
  for pat in "${patterns[@]}"; do
    # pgrep -f returns full-command-line matches; exclude ourselves.
    while read -r pid; do
      [ -z "$pid" ] && continue
      [ "$pid" = "$SELF_PID" ] && continue
      if [ "$sig" = "TERM" ]; then
        local cmd
        cmd=$(ps -p "$pid" -o command= 2>/dev/null | head -c 100)
        echo "  orphan PID $pid: ${cmd}" >&2
        found=1
      fi
      kill "-$sig" "$pid" 2>/dev/null || true
    done < <(pgrep -f "$pat" 2>/dev/null)
  done
}

sweep TERM
if [ "$found" = "1" ]; then
  sleep 1
  sweep KILL  # anything still alive after grace period
fi

exit 0

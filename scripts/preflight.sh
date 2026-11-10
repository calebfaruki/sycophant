#!/usr/bin/env bash
# Step-0 preflight for the sycophant from-scratch build + e2e. Checks the host
# prerequisites that otherwise fail cryptically deep into the build, plus free
# disk, and prints one exact fix line per missing item (macOS + Linux). Exits
# nonzero if anything required is missing, so scripts/e2e.sh aborts before doing
# any expensive work. Safe to run standalone:  scripts/preflight.sh

set -uo pipefail

# Disk thresholds (GB).
VM_DISK_FAIL=8
readonly VM_DISK_FAIL
VM_DISK_WARN=15
readonly VM_DISK_WARN
HOST_DISK_WARN=20
readonly HOST_DISK_WARN

OS="$(uname -s)" # Darwin | Linux
readonly OS
REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
readonly REPO_ROOT

red() { printf '\033[1;31m%s\033[0m' "$*"; }
grn() { printf '\033[1;32m%s\033[0m' "$*"; }
ylw() { printf '\033[1;33m%s\033[0m' "$*"; }

MISSING=0

# need <label> <probe-cmd> <macos-fix> <linux-fix>
# Records a failure (with the OS-appropriate fix) when the probe fails.
need() {
  local label="$1" probe="$2" macfix="$3" linfix="$4"
  if eval "$probe" >/dev/null 2>&1; then
    printf '  %s %s\n' "$(grn '✓')" "$label"
  else
    local fix
    [ "$OS" = "Darwin" ] && fix="$macfix" || fix="$linfix"
    printf '  %s %s\n      fix: %s\n' "$(red '✗')" "$label" "$fix"
    MISSING=$((MISSING + 1))
  fi
}

echo "==> sycophant preflight  (os=${OS})"

# ---- cluster + build tooling on PATH ----
echo "-- toolchain --"
need "docker (running)" "docker info" \
  "open Docker Desktop (or: brew install --cask docker)" \
  "install Docker Engine + start it: https://docs.docker.com/engine/install/"
need "k3d" "command -v k3d" "brew install k3d" \
  "curl -s https://raw.githubusercontent.com/k3d-io/k3d/main/install.sh | bash"
HELM_INSTALL=https://raw.githubusercontent.com/helm/helm/main/scripts/get-helm-3
need "helm" "command -v helm" "brew install helm" \
  "curl -fsSL $HELM_INSTALL | bash"
need "kubectl" "command -v kubectl" "brew install kubectl" \
  "https://kubernetes.io/docs/tasks/tools/#kubectl"
need "cargo" "command -v cargo" "https://rustup.rs" "https://rustup.rs"
need "protoc" "command -v protoc" "brew install protobuf" \
  "apt-get install -y protobuf-compiler  (or: dnf install protobuf-compiler)"
need "cmake" "command -v cmake" "brew install cmake" \
  "apt-get install -y cmake  (or: dnf install cmake)"
need "jq" "command -v jq" "brew install jq" \
  "apt-get install -y jq  (or: dnf install jq)"
need "openssl" "command -v openssl" "brew install openssl" \
  "apt-get install -y openssl"
need "PyJWT" "python3 -c 'import jwt'" "install PyJWT for python3" \
  "install PyJWT for python3"
need "PyYAML" "python3 -c 'import yaml'" "install PyYAML for python3" \
  "install PyYAML for python3"
need "go" "command -v go" "brew install go" "https://go.dev/doc/install"
need "envsubst" "command -v envsubst" "brew install gettext" \
  "apt-get install -y gettext-base"
# Step 5 drives the app path with grpcurl over the run's tailnet.
need "grpcurl" "command -v grpcurl" "brew install grpcurl" \
  "https://github.com/fullstorydev/grpcurl/releases"
need "tailscale" "command -v tailscale" "brew install tailscale" \
  "curl -fsSL https://tailscale.com/install.sh | sh"

# ---- C toolchain (native link step) ----
if [ "$OS" = "Darwin" ]; then
  need "Xcode command-line tools" "xcode-select -p" \
    "xcode-select --install" "n/a"
else
  DNF_DEV="dnf groupinstall 'Development Tools'"
  need "C compiler (cc)" "command -v cc" "n/a" \
    "apt-get install -y build-essential  (or: $DNF_DEV)"
fi

# ---- disk ----
echo "-- disk --"
host_free_kb="$(df -Pk "$REPO_ROOT" 2>/dev/null | awk 'NR==2{print $4}')"
if [ -n "${host_free_kb:-}" ]; then
  host_free_gb=$((host_free_kb / 1024 / 1024))
  if [ "$host_free_gb" -lt "$HOST_DISK_WARN" ]; then
    printf '  %s host free disk: %dGB (< %dGB; ./target is multi-GB)\n' \
      "$(ylw '⚠')" "$host_free_gb" "$HOST_DISK_WARN"
  else
    printf '  %s host free disk: %dGB\n' "$(grn '✓')" "$host_free_gb"
  fi
fi
if docker info >/dev/null 2>&1; then
  vm_free_kb="$(docker run --rm busybox df -Pk / 2>/dev/null |
    awk 'NR==2{print $4}')"
  if [ -n "${vm_free_kb:-}" ]; then
    vm_free_gb=$((vm_free_kb / 1024 / 1024))
    if [ "$vm_free_gb" -lt "$VM_DISK_FAIL" ]; then
      printf '  %s Docker VM free disk: %dGB (< %dGB; kubelet will evict)\n' \
        "$(red '✗')" "$vm_free_gb" "$VM_DISK_FAIL"
      printf '      fix: docker system prune -af\n'
      MISSING=$((MISSING + 1))
    elif [ "$vm_free_gb" -lt "$VM_DISK_WARN" ]; then
      printf '  %s Docker VM free disk: %dGB (< %dGB)\n' \
        "$(ylw '⚠')" "$vm_free_gb" "$VM_DISK_WARN"
    else
      printf '  %s Docker VM free disk: %dGB\n' "$(grn '✓')" "$vm_free_gb"
    fi
  fi
fi

echo
if [ "$MISSING" -gt 0 ]; then
  printf '%s %d required prerequisite(s) missing; fix the ✗ items\n' \
    "$(red 'preflight failed:')" "$MISSING"
  exit 1
fi
printf '%s all required prerequisites present.\n' "$(grn 'preflight ok:')"

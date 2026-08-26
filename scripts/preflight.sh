#!/usr/bin/env bash
# Step-0 preflight for the sycophant from-scratch build + e2e. Checks the host
# prerequisites that otherwise fail cryptically deep into the build, plus free
# disk, and prints one exact fix line per missing item (macOS + Linux). Exits
# nonzero if anything required is missing, so scripts/e2e.sh aborts before doing
# any expensive work. Safe to run standalone:  scripts/preflight.sh
#
# Two tiers:
#   - always:  build + deploy toolchain, free disk
#   - client:  Flutter + platform SDK, checked only when FLUTTER_TARGET != none
#
# Env (shared with scripts/e2e.sh):
#   ARCH            target arch for the musl build (default aarch64)
#   FLUTTER_TARGET  macos | none  (none = backend-only, skips client tier)

set -uo pipefail

ARCH="${ARCH:-aarch64}"
RUST_TARGET="${ARCH}-unknown-linux-musl"
CROSS_LINKER="${ARCH}-linux-musl-gcc"
FLUTTER_TARGET="${FLUTTER_TARGET:-macos}"

# Disk thresholds (GB).
VM_DISK_FAIL=8
VM_DISK_WARN=15
HOST_DISK_WARN=20

OS="$(uname -s)"   # Darwin | Linux
REPO_ROOT="$(git rev-parse --show-toplevel 2>/dev/null || pwd)"

red()  { printf '\033[1;31m%s\033[0m' "$*"; }
grn()  { printf '\033[1;32m%s\033[0m' "$*"; }
ylw()  { printf '\033[1;33m%s\033[0m' "$*"; }

MISSING=0

# need <label> <probe-cmd> <macos-fix> <linux-fix>
# Records a failure (with the OS-appropriate fix) when the probe fails.
need() {
  local label="$1" probe="$2" macfix="$3" linfix="$4"
  if eval "$probe" >/dev/null 2>&1; then
    printf '  %s %s\n' "$(grn '✓')" "$label"
  else
    local fix; [ "$OS" = "Darwin" ] && fix="$macfix" || fix="$linfix"
    printf '  %s %s\n      fix: %s\n' "$(red '✗')" "$label" "$fix"
    MISSING=$((MISSING + 1))
  fi
}

echo "==> sycophant preflight  (arch=${ARCH}, client=${FLUTTER_TARGET}, os=${OS})"

# ---- always: cluster + build tooling on PATH ----
echo "-- toolchain --"
need "docker (running)" "docker info" \
  "open Docker Desktop (or: brew install --cask docker)" \
  "install Docker Engine + start it: https://docs.docker.com/engine/install/"
need "k3d"     "command -v k3d"     "brew install k3d"      "curl -s https://raw.githubusercontent.com/k3d-io/k3d/main/install.sh | bash"
need "helm"    "command -v helm"    "brew install helm"     "curl -fsSL https://raw.githubusercontent.com/helm/helm/main/scripts/get-helm-3 | bash"
need "kubectl" "command -v kubectl" "brew install kubectl"  "https://kubernetes.io/docs/tasks/tools/#kubectl"
need "cargo"   "command -v cargo"   "https://rustup.rs"     "https://rustup.rs"
need "protoc"  "command -v protoc"  "brew install protobuf" "apt-get install -y protobuf-compiler  (or: dnf install protobuf-compiler)"
need "cmake"   "command -v cmake"   "brew install cmake"    "apt-get install -y cmake  (or: dnf install cmake)"

# ---- always: C toolchain (native link step) ----
if [ "$OS" = "Darwin" ]; then
  need "Xcode command-line tools" "xcode-select -p" \
    "xcode-select --install" "n/a"
else
  need "C compiler (cc)" "command -v cc" \
    "n/a" "apt-get install -y build-essential  (or: dnf groupinstall 'Development Tools')"
fi

# ---- always: musl cross-build chain for ${RUST_TARGET} ----
echo "-- musl cross-build (${RUST_TARGET}) --"
need "rustup target ${RUST_TARGET}" \
  "rustup target list --installed | grep -qx ${RUST_TARGET}" \
  "rustup target add ${RUST_TARGET}" \
  "rustup target add ${RUST_TARGET}"
need "cross-linker ${CROSS_LINKER}" \
  "command -v ${CROSS_LINKER}" \
  "brew install messense/macos-cross-toolchains/${ARCH}-unknown-linux-musl" \
  "install a ${ARCH}-linux-musl cross toolchain (e.g. musl-cross) providing ${CROSS_LINKER}"
# The target + compiler are useless to cargo without the linker wired in
# ~/.cargo/config.toml; the build links against the host cc otherwise and fails.
need "~/.cargo/config.toml linker for ${RUST_TARGET}" \
  "grep -q 'target.${RUST_TARGET}' \"\${CARGO_HOME:-\$HOME/.cargo}/config.toml\"" \
  "add to ~/.cargo/config.toml:  [target.${RUST_TARGET}]\\n             linker = \"${CROSS_LINKER}\"" \
  "add to ~/.cargo/config.toml:  [target.${RUST_TARGET}]\\n             linker = \"${CROSS_LINKER}\""

# ---- client tier: only when a client is being installed ----
if [ "$FLUTTER_TARGET" != "none" ]; then
  echo "-- flutter client (FLUTTER_TARGET=${FLUTTER_TARGET}) --"
  need "flutter" "command -v flutter" \
    "brew install --cask flutter" \
    "https://docs.flutter.dev/get-started/install/linux"
  case "$FLUTTER_TARGET" in
    macos)
      need "full Xcode (macOS app build)" "xcodebuild -version" \
        "install Xcode from the App Store, then: sudo xcode-select -s /Applications/Xcode.app && sudo xcodebuild -license accept" \
        "n/a (macOS target needs macOS)"
      need "CocoaPods" "command -v pod" "brew install cocoapods" "n/a"
      ;;
    *)
      printf '  %s unknown FLUTTER_TARGET=%s (expected macos|none)\n' "$(red '✗')" "$FLUTTER_TARGET"
      MISSING=$((MISSING + 1))
      ;;
  esac
else
  echo "-- flutter client: skipped (FLUTTER_TARGET=none, backend-only) --"
fi

# ---- disk ----
echo "-- disk --"
host_free_kb="$(df -Pk "$REPO_ROOT" 2>/dev/null | awk 'NR==2{print $4}')"
if [ -n "${host_free_kb:-}" ]; then
  host_free_gb=$((host_free_kb / 1024 / 1024))
  if [ "$host_free_gb" -lt "$HOST_DISK_WARN" ]; then
    printf '  %s host free disk: %dGB (< %dGB; the ./target build is multi-GB)\n' "$(ylw '⚠')" "$host_free_gb" "$HOST_DISK_WARN"
  else
    printf '  %s host free disk: %dGB\n' "$(grn '✓')" "$host_free_gb"
  fi
fi
if docker info >/dev/null 2>&1; then
  vm_free_kb="$(docker run --rm busybox df -Pk / 2>/dev/null | awk 'NR==2{print $4}')"
  if [ -n "${vm_free_kb:-}" ]; then
    vm_free_gb=$((vm_free_kb / 1024 / 1024))
    if [ "$vm_free_gb" -lt "$VM_DISK_FAIL" ]; then
      printf '  %s Docker VM free disk: %dGB (< %dGB — kubelet will evict; reclaim with `docker system prune -af`)\n' "$(red '✗')" "$vm_free_gb" "$VM_DISK_FAIL"
      MISSING=$((MISSING + 1))
    elif [ "$vm_free_gb" -lt "$VM_DISK_WARN" ]; then
      printf '  %s Docker VM free disk: %dGB (< %dGB; consider `docker system prune -af`)\n' "$(ylw '⚠')" "$vm_free_gb" "$VM_DISK_WARN"
    else
      printf '  %s Docker VM free disk: %dGB\n' "$(grn '✓')" "$vm_free_gb"
    fi
  fi
fi

echo
if [ "$MISSING" -gt 0 ]; then
  printf '%s %d required prerequisite(s) missing — fix the ✗ items above and re-run.\n' "$(red 'preflight failed:')" "$MISSING"
  exit 1
fi
printf '%s all required prerequisites present.\n' "$(grn 'preflight ok:')"

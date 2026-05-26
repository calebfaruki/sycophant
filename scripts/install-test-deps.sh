#!/usr/bin/env bash
# Idempotent installer for the K8s test harness binaries (chainsaw + kyverno CLI).
# Skips installs when binaries are already on PATH at the right version.
# CI sets CI=true; we install unconditionally then because PATH state is fresh.
#
# Both binaries are pulled as prebuilt release tarballs — no Go toolchain needed.
# Override the install destination with `BIN_DIR=...`. Defaults to `~/.local/bin`,
# which must be on your PATH (most modern shells include it by default).
set -euo pipefail

CHAINSAW_VERSION="${CHAINSAW_VERSION:-0.2.15}"
KYVERNO_CLI_VERSION="${KYVERNO_CLI_VERSION:-1.13.4}"
BIN_DIR="${BIN_DIR:-$HOME/.local/bin}"

mkdir -p "$BIN_DIR"

need_install() {
  local bin="$1" want="$2"
  if [[ "${CI:-}" == "true" ]]; then return 0; fi
  if ! command -v "$bin" >/dev/null 2>&1; then return 0; fi
  local have
  have="$("$bin" version 2>/dev/null | grep -oE '[0-9]+\.[0-9]+\.[0-9]+' | head -n1 || true)"
  [[ "$have" == "$want" ]] && return 1 || return 0
}

platform() {
  local os arch
  os="$(uname -s | tr '[:upper:]' '[:lower:]')"
  arch="$(uname -m)"
  case "$arch" in
    x86_64) arch=amd64 ;;
    aarch64) arch=arm64 ;;
  esac
  echo "${os}_${arch}"
}

install_chainsaw() {
  if need_install chainsaw "$CHAINSAW_VERSION"; then
    echo ">>> installing chainsaw $CHAINSAW_VERSION → $BIN_DIR"
    local plat tmpdir
    plat="$(platform)"
    tmpdir="$(mktemp -d)"
    curl -fsSL -o "$tmpdir/chainsaw.tar.gz" \
      "https://github.com/kyverno/chainsaw/releases/download/v${CHAINSAW_VERSION}/chainsaw_${plat}.tar.gz"
    tar -xzf "$tmpdir/chainsaw.tar.gz" -C "$tmpdir"
    install -m 0755 "$tmpdir/chainsaw" "$BIN_DIR/chainsaw"
    rm -rf "$tmpdir"
  else
    echo ">>> chainsaw $CHAINSAW_VERSION already on PATH"
  fi
}

install_kyverno_cli() {
  if need_install kyverno "$KYVERNO_CLI_VERSION"; then
    echo ">>> installing kyverno CLI $KYVERNO_CLI_VERSION → $BIN_DIR"
    local plat tmpdir
    plat="$(platform)"
    tmpdir="$(mktemp -d)"
    curl -fsSL -o "$tmpdir/kyverno-cli.tar.gz" \
      "https://github.com/kyverno/kyverno/releases/download/v${KYVERNO_CLI_VERSION}/kyverno-cli_v${KYVERNO_CLI_VERSION}_${plat}.tar.gz"
    tar -xzf "$tmpdir/kyverno-cli.tar.gz" -C "$tmpdir"
    install -m 0755 "$tmpdir/kyverno" "$BIN_DIR/kyverno"
    rm -rf "$tmpdir"
  else
    echo ">>> kyverno CLI $KYVERNO_CLI_VERSION already on PATH"
  fi
}

install_chainsaw
install_kyverno_cli

case ":$PATH:" in
  *":$BIN_DIR:"*) ;;
  *) echo "warning: $BIN_DIR is not on your PATH — add it or export BIN_DIR to a directory that is." ;;
esac

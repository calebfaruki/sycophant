#!/usr/bin/env bash
# Idempotent installer for the test and lint binaries (chainsaw, kyverno
# CLI, shellcheck, shfmt, editorconfig-checker). Skips installs when binaries
# are already on PATH at the right version.
#
# All five are prebuilt release files; nothing is compiled. Override the
# install destination with `BIN_DIR=...`. Defaults to `~/.local/bin`, which
# must be on your PATH (most modern shells include it by default).
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
readonly REPO_ROOT
source "$REPO_ROOT/lib/shell.sh"

readonly CHAINSAW_VERSION=0.2.15
readonly KYVERNO_CLI_VERSION=1.19.1
readonly SHELLCHECK_VERSION=0.11.0
readonly SHFMT_VERSION=3.14.1
readonly EC_VERSION=4.0.2
# The release files' sha256 per platform, for the versions above.
readonly CHAINSAW_SHA256="
linux_amd64 295d226c89f126c0a97775d364be149f47a810c8a3f9829ee410583d0c1abe3c
linux_arm64 fecf5f3ea74d11da2cd5d95a4c016d5d2543cbc4844ce21adf5e727e7ffd9e44
darwin_amd64 ddb56002ff8c6265f55ec40c82eb0ef0dddec5127dd12d4fc5e7953453481f4b
darwin_arm64 502bef17dd822217e82590e9ccf01c1e26a56b3fa34a835789b9af73e82a169a"
readonly KYVERNO_SHA256="
linux_amd64 b38228f367fc0fdc2b08f4c83ea50ac5f16c60ff8d62d76a66157c33c47b70ae
linux_arm64 d78fecd183e1e6749c653fdb744c88d065e6737ec6856c97c4efac84bec54554
darwin_amd64 d9d7a857755ccd027cba8b2a5146a515c1590d65b38cb37a2170809329e4f393
darwin_arm64 2cf5febbedaafca5d3c7819925a49f9723bf4c3337a1327b6f72970480af385a"
readonly SHELLCHECK_SHA256="
linux_amd64 b7af85e41cc99489dcc21d66c6d5f3685138f06d34651e6d34b42ec6d54fe6f6
linux_arm64 68a8133197a50beb8803f8d42f9908d1af1c5540d4bb05fdfca8c1fa47decefc
darwin_amd64 c2c15e08df0e8fbc374c335b230a7ee958c313fa5714817a59aa59f1aa594f51
darwin_arm64 339b930feb1ea764467013cc1f72d09cd6b869ebf1013296ba9055ab2ffbd26f"
readonly SHFMT_SHA256="
linux_amd64 76e77641faa025814b77f153b29796b8e6fa2fca03e0c76a691608b86c7ea7bf
linux_arm64 5f2db09dae91fca848f7adbdd014632e921a383863a2ad7e0450ad3aba0c6489
darwin_amd64 d33eee0da0f92835b3562e9767a05cee7e4eaeef47daa03bfd09da17b4b590a6
darwin_arm64 b7c872db63553ccffc7253aba3ed7d4885a27d83f1ba567b1138c6315a5847e5"
readonly EC_SHA256="
linux_amd64 44efaef3ed14c6ab52eb87481d78e5e5c204796dccf26e2f4598a61ba17f8003
linux_arm64 abad76da8c9950fdc2a314bcd66969172aa823e9c595fa7e3a389a91e34d025e
darwin_amd64 ff59f57717d95fc62b8e372c8051dbbb766f7d660942409a85d027b0feb2422c
darwin_arm64 ff59f57717d95fc62b8e372c8051dbbb766f7d660942409a85d027b0feb2422c"
BIN_DIR="${BIN_DIR:-$HOME/.local/bin}"
readonly BIN_DIR

mkdir -p "$BIN_DIR"

need_install() {
  local bin="$1" want="$2" arg="$3"
  if ! command -v "$bin" >/dev/null 2>&1; then return 0; fi
  local have
  have="$("$bin" "$arg" 2>/dev/null | grep -oE '[0-9]+\.[0-9]+\.[0-9]+' |
    head -n1 || true)"
  [[ "$have" != "$want" ]]
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

# fetch <url> <file> <sums>: download, then refuse the file unless its sha256
# is the one <sums> pins for this platform.
fetch() {
  local url="$1" file="$2" sums="$3" want have
  want="$(awk -v p="$(platform)" '$1 == p {print $2}' <<<"$sums")"
  curl -fsSL -o "$file" "$url"
  if command -v sha256sum >/dev/null 2>&1; then
    have="$(sha256sum "$file" | cut -d' ' -f1)"
  else
    have="$(shasum -a 256 "$file" | cut -d' ' -f1)"
  fi
  if [[ -z "$want" || "$have" != "$want" ]]; then
    echo "error: $url has sha256 $have, want ${want:-a pinned sum}" >&2
    exit 1
  fi
}

# install_bin <bin> <version> <version-arg> <url> <sums> [member]: install
# <bin> from <url> unless it is on PATH at <version>. With a member, <url> is
# a tarball and the binary is <member> inside it.
install_bin() {
  local bin="$1" version="$2" arg="$3" url="$4" sums="$5" member="${6:-}"
  if ! need_install "$bin" "$version" "$arg"; then
    echo ">>> $bin $version already on PATH"
    return
  fi
  echo ">>> installing $bin $version → $BIN_DIR"
  local tmpdir
  tmpdir="$(mktemp -d)"
  fetch "$url" "$tmpdir/download" "$sums"
  if [ -n "$member" ]; then
    tar -xzf "$tmpdir/download" -C "$tmpdir"
  fi
  install -m 0755 "$tmpdir/${member:-download}" "$BIN_DIR/$bin"
  rm -rf "$tmpdir"
  if need_install "$bin" "$version" "$arg"; then
    echo "warning: $(command -v "$bin" || echo "$bin") is not $version;" \
      "put $BIN_DIR first on PATH"
  fi
}

v="$CHAINSAW_VERSION"
url="https://github.com/kyverno/chainsaw/releases/download/v$v/chainsaw_$(platform).tar.gz" # editorconfig-checker-disable-line
install_bin chainsaw "$v" version "$url" "$CHAINSAW_SHA256" chainsaw

# Kyverno names amd64 x86_64.
v="$KYVERNO_CLI_VERSION"
asset="$(platform | sed 's/amd64$/x86_64/')"
url="https://github.com/kyverno/kyverno/releases/download/v$v/kyverno-cli_v${v}_${asset}.tar.gz" # editorconfig-checker-disable-line
install_bin kyverno "$v" version "$url" "$KYVERNO_SHA256" kyverno

# ShellCheck names its files <os>.<arch>, with x86_64 and aarch64.
v="$SHELLCHECK_VERSION"
asset="$(platform | sed 's/_amd64$/.x86_64/; s/_arm64$/.aarch64/')"
url="https://github.com/koalaman/shellcheck/releases/download/v$v/shellcheck-v$v.${asset}.tar.gz" # editorconfig-checker-disable-line
install_bin shellcheck "$v" --version "$url" "$SHELLCHECK_SHA256" \
  "shellcheck-v$v/shellcheck"

v="$SHFMT_VERSION"
url="https://github.com/mvdan/sh/releases/download/v$v/shfmt_v${v}_$(platform)" # editorconfig-checker-disable-line
install_bin shfmt "$v" --version "$url" "$SHFMT_SHA256"

# One universal build serves both macOS arches.
v="$EC_VERSION"
case "$(platform)" in
  darwin_*) asset=darwin-all ;;
  *) asset="$(platform | tr _ -)" ;;
esac
url="https://github.com/editorconfig-checker/editorconfig-checker/releases/download/v$v/editorconfig-checker-${asset}.tar.gz" # editorconfig-checker-disable-line
install_bin editorconfig-checker "$v" --version "$url" "$EC_SHA256" \
  editorconfig-checker

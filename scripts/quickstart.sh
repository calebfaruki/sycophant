#!/usr/bin/env bash
# Run the README CLI quickstart end to end with the hello-world examples, then
# open `syco chat`. Re-running reuses the cluster and re-applies each piece.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
readonly REPO_ROOT
source "$REPO_ROOT/lib/shell.sh"
source "$REPO_ROOT/lib/local-cluster.sh"

readonly EX="$REPO_ROOT/examples/sycoadm"

[ -f "$GGUF_PATH" ] || {
  warn "model weights not found at $GGUF_PATH"
  exit 1
}

step "Building sycoadm and syco"
make -C "$REPO_ROOT" sycoadm syco
export PATH="$REPO_ROOT/target:$PATH"

# Only sycoadm gets the shared dir: syco reads SYCO_CONFIG_DIR too.
sycoadm() { SYCO_CONFIG_DIR="$OPERATOR_CONFIG_DIR" command sycoadm "$@"; }

saved_email="$OPERATOR_CONFIG_DIR/oidc/operator-email"
if [ -z "${SYCO_EMAIL:-}" ] && [ -s "$saved_email" ]; then
  SYCO_EMAIL="$(cat "$saved_email")"
fi
export SYCO_EMAIL="${SYCO_EMAIL:-$OPERATOR_EMAIL}"
syco_credentials

step "Cluster"
sycoadm setup

step "Tenant"
sycoadm tenant up -f "$EX/tenants/hello-world.yaml"

step "Inference images"
if ! docker image inspect llama-server:local >/dev/null 2>&1; then
  docker pull -q "$LLAMA_SERVER_REF"
  docker tag "$LLAMA_SERVER_REF" llama-server:local
fi
if ! docker image inspect weights:local >/dev/null 2>&1; then
  build_weights_image
fi
ok "llama-server:local and weights:local present"

step "Inference, toolset, demo secret"
sycoadm inference up -f "$EX/inference/qwen3-5-2b.yaml"
sycoadm toolset up -f "$EX/toolsets/hello-world/values.yaml"
printf '%s' "$DEMO_SECRET" |
  sycoadm tenant secret set demo-ssh-key --tenant hello-world

step "Agent, instructions, channel"
sycoadm agent up -f "$EX/agents/hello-world.yaml"
sycoadm instructions up -f "$EX/instructions/hello-world.yaml"
sycoadm channel up -f "$EX/channels/tailnet.yaml"

step "Tailnet join (needs sudo)"
sycoadm connect --tenant hello-world

step "syco login and agent binding"
syco_dir="${SYCO_CONFIG_DIR:-$HOME/.config/syco}"
if [ ! -f "$syco_dir/config.toml" ]; then
  mkdir -p "$syco_dir"
  sed "s/\${TAILNET_RELAY_ADDR}/$TAILNET_RELAY_ADDR/" \
    "$REPO_ROOT/examples/syco/config.toml" >"$syco_dir/config.toml"
  ok "wrote $syco_dir/config.toml"
fi
syco login --email "$SYCO_EMAIL"
sub="$(syco whoami | awk '/^Subject:/ {print $2}')"
[ -n "$sub" ] || {
  warn "could not read the subject from syco whoami"
  exit 1
}
sycoadm user up -f "$EX/users/alice.yaml" --set "user.subject=$DEX_ISSUER#$sub"

step "syco chat"
exec syco chat

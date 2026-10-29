#!/usr/bin/env bash
# Run the README CLI quickstart end to end with the hello-world examples, then
# open `syco chat`. Re-running reuses the cluster and re-applies each piece.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
readonly REPO_ROOT
source "$REPO_ROOT/lib/shell.sh"

readonly EX="$REPO_ROOT/examples/sycoadm"
readonly GGUF_PATH="${GGUF_PATH:-$HOME/.cache/sycophant/weights/Qwen3.5-2B-Q4_K_M.gguf}"
readonly LLAMA_SERVER_REF="ghcr.io/ggml-org/llama.cpp:server@sha256:9f84380be42d6285a827629c809387349c3541aa8986f7536547ca33cc8dd47a"
readonly ISSUER="https://127.0.0.1:30443/dex"

[ -f "$GGUF_PATH" ] || { warn "model weights not found at $GGUF_PATH"; exit 1; }

step "Building sycoadm and syco"
make -C "$REPO_ROOT" sycoadm syco
export PATH="$REPO_ROOT/target:$PATH"

# Share the operator state `make test-e2e` uses, so either one can reuse the
# cluster the other created: the Dex trust lives in this config dir, and a
# cluster set up from a different dir rejects the login.
readonly ADM_CONFIG_DIR="$REPO_ROOT/tmp/syco-config"
readonly PASSWORD_FILE="$REPO_ROOT/tmp/e2e-operator-password"
sycoadm() { SYCO_CONFIG_DIR="$ADM_CONFIG_DIR" command sycoadm "$@"; }

saved_email="$ADM_CONFIG_DIR/oidc/operator-email"
if [ -z "${SYCO_EMAIL:-}" ] && [ -s "$saved_email" ]; then
  SYCO_EMAIL="$(cat "$saved_email")"
fi
export SYCO_EMAIL="${SYCO_EMAIL:-e2e-operator@sycophant.local}"
if [ -z "${SYCO_PASSWORD:-}" ]; then
  if [ ! -s "$PASSWORD_FILE" ]; then
    mkdir -p "$(dirname "$PASSWORD_FILE")"
    ( umask 077; head -c 256 /dev/urandom | LC_ALL=C tr -dc 'a-zA-Z0-9' | cut -c1-24 > "$PASSWORD_FILE" )
  fi
  SYCO_PASSWORD="$(cat "$PASSWORD_FILE")"
fi
export SYCO_PASSWORD

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
  docker build -q -f "$REPO_ROOT/build/weights.Dockerfile" \
    --build-arg "GGUF=$(basename "$GGUF_PATH")" \
    -t weights:local "$(dirname "$GGUF_PATH")"
fi
ok "llama-server:local and weights:local present"

step "Inference, toolset, demo secret"
sycoadm inference up -f "$EX/inference/qwen3-5-2b.yaml"
sycoadm toolset up -f "$EX/toolsets/hello-world/values.yaml"
printf '%s' 'hello-world-demo-secret-DO-NOT-USE' \
  | sycoadm tenant secret set demo-ssh-key --ns tenant-hello-world

step "Workspace, instructions, channel"
sycoadm workspace up -f "$EX/workspaces/hello-world.yaml"
sycoadm instructions up -f "$EX/instructions/hello-world.yaml"
sycoadm channel up -f "$EX/channels/tailnet.yaml"

step "Tailnet join (needs sudo)"
sycoadm connect --tenant hello-world

step "syco login and workspace binding"
syco_dir="${SYCO_CONFIG_DIR:-$HOME/.config/syco}"
if [ ! -f "$syco_dir/config.toml" ]; then
  mkdir -p "$syco_dir"
  sed 's/${TAILNET_RELAY_ADDR}/relay:9090/' "$REPO_ROOT/examples/syco/config.toml" > "$syco_dir/config.toml"
  ok "wrote $syco_dir/config.toml"
fi
syco login --email "$SYCO_EMAIL"
sub="$(syco whoami | awk '/^Subject:/ {print $2}')"
[ -n "$sub" ] || { warn "could not read the subject from syco whoami"; exit 1; }
sycoadm user up -f "$EX/users/alice.yaml" --set "user.subject=$ISSUER#$sub"

step "syco chat"
exec syco chat

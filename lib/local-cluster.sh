# shellcheck shell=bash disable=SC2034 # the sourcing scripts read these
# lib/local-cluster.sh: the inputs scripts/quickstart.sh and scripts/e2e.sh
# share. Change them with README.md, examples/sycoadm/users/alice.yaml and
# cli/src/commands/operator.rs.

# Source: https://huggingface.co/unsloth/Qwen3.5-2B-GGUF
GGUF_PATH="${GGUF_PATH:-$HOME/.cache/sycophant/weights/Qwen3.5-2B-Q4_K_M.gguf}"
readonly GGUF_PATH
# llama-server is a third-party engine: pulled by digest, never built here.
# editorconfig-checker-disable-next-line
LLAMA_SERVER_REF="${LLAMA_SERVER_REF:-ghcr.io/ggml-org/llama.cpp:server@sha256:9f84380be42d6285a827629c809387349c3541aa8986f7536547ca33cc8dd47a}"
readonly LLAMA_SERVER_REF
# The bundled Dex. The apiserver only accepts an https issuer, and k3s runs
# kube-apiserver in the node netns with no CoreDNS, so Dex is published on a
# pinned NodePort the apiserver reaches at the node loopback.
readonly DEX_ISSUER="https://127.0.0.1:30443/dex"
# The client reaches the relay through the tailnet channel's node, at the
# channel's MagicDNS hostname. Nothing dials the relay's app port directly.
readonly TAILNET_RELAY_ADDR="relay:9090"

# Either script reuses the cluster the other created: the Dex trust lives in
# this config dir, and a cluster set up from a different dir rejects the login.
readonly OPERATOR_CONFIG_DIR="$REPO_ROOT/tmp/syco-config"
readonly OPERATOR_EMAIL="e2e-operator@sycophant.local"
# Dex's operator login. The password persists for reuse runs against the same
# Dex; e2e's bootstrap regenerates it with the cluster.
readonly SYCO_PASSWORD_FILE="$REPO_ROOT/tmp/e2e-operator-password"
# The demo secret value `sycoadm tenant secret set demo-ssh-key` stores and the
# agent's provision gives toolset hello-world. Single-line: the scrubber redacts
# single-line values.
readonly DEMO_SECRET="hello-world-demo-secret-DO-NOT-USE"

# Export SYCO_PASSWORD: the caller's, else the persisted one, else a new one.
syco_credentials() {
  if [ -z "${SYCO_PASSWORD:-}" ]; then
    if [ ! -s "$SYCO_PASSWORD_FILE" ]; then
      mkdir -p "$(dirname "$SYCO_PASSWORD_FILE")"
      (
        umask 077
        head -c 256 /dev/urandom | LC_ALL=C tr -dc 'a-zA-Z0-9' |
          cut -c1-24 >"$SYCO_PASSWORD_FILE"
      )
    fi
    SYCO_PASSWORD="$(cat "$SYCO_PASSWORD_FILE")"
  fi
  export SYCO_PASSWORD
}

# Build weights:local, the GGUF_PATH model in a data image.
build_weights_image() {
  docker build -q -f "$REPO_ROOT/build/weights.Dockerfile" \
    --build-arg "GGUF=$(basename "$GGUF_PATH")" \
    -t weights:local "$(dirname "$GGUF_PATH")"
}

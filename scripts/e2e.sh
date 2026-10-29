#!/usr/bin/env bash
# Single-command end-to-end test for sycophant.
#
# The mechanical hello world: installs every piece through sycoadm from the
# example values files under examples/sycoadm, drives the app path over the
# run's tailnet with the operator's Dex token (syco CLI), and runs the Step 6
# security assertions. Unattended apart from one sudo prompt for the tailnet
# join.
#
# By default reuses an existing k3d cluster and its loaded images, running only
# the phases that prove the cluster works. Pass --clean to delete+recreate the
# cluster and rebuild + load all images from scratch. With no cluster present,
# the first run bootstraps once regardless of --clean.
#
# Usage:
#   scripts/e2e.sh [--clean]   (--clean = fresh cluster + full image rebuild)
#
# Optional env:
#   CLUSTER_NAME (default sycophant),
#   ARCH (default aarch64), DOCKER_ARCH (default arm64), and the SYCO_* inputs
#   `sycoadm setup` and the tenant commands read (see "sycoadm inputs" below). sycoadm's
#   config root is SYCO_CONFIG_DIR (default tmp/syco-config), never ~/.config.
#
# A cluster without the Dex operator login needs one run with --clean.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
readonly REPO_ROOT
source "$REPO_ROOT/lib/shell.sh"
CLUSTER_NAME="${CLUSTER_NAME:-sycophant}"
readonly CLUSTER_NAME
# The main tenant, as examples/sycoadm/tenants/hello-world.yaml names it.
readonly NAMESPACE="tenant-hello-world"
ARCH="${ARCH:-aarch64}"
readonly ARCH
DOCKER_ARCH="${DOCKER_ARCH:-arm64}"
readonly DOCKER_ARCH
RUST_TARGET="${ARCH}-unknown-linux-musl"
readonly RUST_TARGET

# ---- flags ----
# --clean forces a fresh cluster + full image rebuild. Default reuses an
# existing cluster and its loaded images, running only the deploy + verify +
# security phases that prove the cluster works. With no cluster present, the
# first run bootstraps once regardless.
CLEAN=0
for arg in "$@"; do
  case "$arg" in
    --clean) CLEAN=1 ;;
    -h|--help)
      printf 'Usage: %s [--clean]\n  (default) reuse the existing cluster + images; run deploy/verify/security\n  --clean   delete+recreate the cluster and rebuild + load all images\n' "$(basename "$0")"
      exit 0 ;;
    *) printf 'unknown argument: %s (see --help)\n' "$arg" >&2; exit 1 ;;
  esac
done
readonly CLEAN
# The app path is driven by the operator's person token: a Dex-signed ID
# token whose subject `sycoadm user up` bound to the workspace list. The relay
# mints nothing; the operator's live RBAC is the whole authorization.
#
# The client reaches the relay through the tailnet channel's node, at the
# channel's MagicDNS hostname. Nothing dials the relay's app port directly.
export TAILNET_RELAY_ADDR="relay:9090"
readonly TAILNET_RELAY_ADDR
# The gateway serves no reflection: grpcurl reads the service from the protos.
readonly RELAY_GRPC_PROTO="-import-path $REPO_ROOT/crates/relay-proto/proto -import-path $REPO_ROOT/crates/proto-common/proto -proto relay/v1/relay.proto"
# RCLONE_IMAGE (the harness sync client, a neutral S3 client run against the
# SeaweedFS s3 gateway) is a tenant workload image loaded in step_1_build. It is
# a multi-arch index: export one arch to a tar to import.
readonly RCLONE_IMAGE="rclone/rclone:1.75.1"
# Marker the macOS tailnet join writes immediately before its sudo prompt and
# removes once the join returns. An external monitor watches this path to alert
# the operator that a password entry is imminent (the run is otherwise headless
# until Step 5).
readonly SUDO_PENDING_SENTINEL="/tmp/sycophant-e2e-sudo-pending"
# The in-cluster inference model key. Its `server` block makes the chart
# render the llama.cpp Deployment, its Service and its ingress fence (CNP
# inf-<key>). Every local model serves on the fixed port 8080.
readonly INFERENCE_PROFILE="qwen3-5-2b"
readonly INFERENCE_PORT="8080"
# The demo secret value `sycoadm tenant secret set demo-ssh-key` stores and the
# workspace grants as hello-world/demo-key. Single-line: the scrubber redacts
# single-line values.
readonly DEMO_SECRET="hello-world-demo-secret-DO-NOT-USE"

# ---- ui helpers ----
# step, ok, warn are provided by lib/shell.sh (sourced above).
# wait_for <description> <timeout-seconds> <shell-condition>
# Polls the condition every 2 s until it succeeds OR the timeout expires.
# Returns non-zero on timeout, with a `warn` log. Use this instead of an
# unbounded `until ... ; do sleep N; done` so hangs surface as failures.
wait_for() {
  local desc="$1"; local timeout="$2"; shift 2
  local deadline
  deadline=$(( $(date +%s) + timeout ))
  while ! eval "$*"; do
    if [ "$(date +%s)" -ge "$deadline" ]; then
      warn "timeout after ${timeout}s waiting for: ${desc}"
      return 1
    fi
    sleep 2
  done
}

# Decode a single claim from a compact JWT (no verification).
jwt_claim() {
  local token="$1" claim="$2"
  printf '%s' "$token" \
    | cut -d. -f2 \
    | tr '\-_' '+/' \
    | awk '{ m = length($0) % 4; pad = ""; if (m) for (i = 0; i < 4 - m; i++) pad = pad "="; print $0 pad }' \
    | base64 -d 2>/dev/null \
    | jq -r ".${claim}"
}

# assert_path_absent <pod> <namespace> <path> <desc>
# The file must NOT exist inside the pod.
assert_path_absent() {
  local pod="$1" ns="$2" path="$3" desc="$4"
  if kubectl exec -n "$ns" "$pod" -- test -e "$path" >/dev/null 2>&1; then
    warn "${path} exists inside ${pod}: ${desc}"
    exit 1
  fi
}

# ---- background cleanup (background sycoadm runs) ----
declare -a CLEANUP_PIDS=()
cleanup() {
  local pid
  for pid in "${CLEANUP_PIDS[@]:-}"; do
    [ -n "$pid" ] && kill "$pid" 2>/dev/null || true
  done
}
trap cleanup EXIT

# Defensive: kill leftover sycophant processes from prior runs (leaked
# cargo test binaries, abandoned kubectl port-forwards, respawn loops).
"$REPO_ROOT/scripts/kill-orphans.sh"

# ---- step 0: bootstrap ----
# `sycoadm setup` does the operator's bootstrap: k3d cluster, gVisor, Cilium,
# CoreDNS registry wiring, Kyverno, the cluster chart, and Dex with the operator
# as its one password user.
sycoadm_setup() {
  syco_credentials
  "$SYCO" setup
  ok "sycoadm setup: cluster, gVisor, Cilium, Kyverno, cluster chart, Dex, object store"
}

step_0_bootstrap() {
  step "Step 0: Bootstrap cluster (sycoadm setup)"

  k3d cluster delete "$CLUSTER_NAME" 2>/dev/null || true
  rm -f "$SYCO_PASSWORD_FILE"
  sycoadm_setup
}

# Import a single-arch image from a multi-arch index into k3d.
# $1 = upstream ref, $2 = optional local tag.
import_multiarch_image() {
  local ref="$1" local_tag="${2:-}"
  docker pull -q --platform "linux/${DOCKER_ARCH}" "$ref" >/dev/null
  [ -z "$local_tag" ] || docker tag "$ref" "$local_tag"
  local save_ref="${local_tag:-$ref}"
  local tar; tar="$(mktemp -t image.XXXXXX).tar"
  docker image save --platform "linux/${DOCKER_ARCH}" -o "$tar" "$save_ref"
  k3d image import "$tar" --cluster "$CLUSTER_NAME" >/dev/null
  rm -f "$tar"
}

# ---- step 1: build images ----
build_rust_binaries() {
  cargo build --release --target "$RUST_TARGET" \
    -p inference-runtime \
    -p toolset-runtime \
    -p harness -p relay-controller
}

build_go_binary() {
  # Go syco CLI binary, used in Step 5 to drive the app path.
  (cd "$REPO_ROOT/cmd/syco" && go build -o "$REPO_ROOT/target/syco" .) \
    || { warn "go build of cmd/syco failed"; return 1; }
  ok "Built the Go syco binary"
}

build_docker_images() {
  local bin
  for bin in toolset-runtime relay-controller inference-runtime; do
    cp "target/$RUST_TARGET/release/$bin" "${bin}-linux-musl-${DOCKER_ARCH}"
    docker build -q -f build/Dockerfile \
      --build-arg "BINARY=$bin" --build-arg "TARGETARCH=$DOCKER_ARCH" \
      -t "${bin}:local" . >/dev/null
    rm "${bin}-linux-musl-${DOCKER_ARCH}"
  done

  # The one toolset base image: its entrypoint is the toolset runtime. The
  # stdlib `toolset` image builds FROM it; `toolset-hello-world` builds FROM
  # `toolset:local`, so the loop order below matters.
  cp "target/$RUST_TARGET/release/toolset-runtime" "images/toolset-base/toolset-runtime-linux-${DOCKER_ARCH}"
  docker build -q --build-arg "TARGETARCH=$DOCKER_ARCH" -f images/toolset-base/Dockerfile images/toolset-base/ -t toolset-base:local >/dev/null
  rm "images/toolset-base/toolset-runtime-linux-${DOCKER_ARCH}"

  cp "target/$RUST_TARGET/release/harness" "harness-linux-musl-${DOCKER_ARCH}"
  docker build -q -f build/Dockerfile \
    --build-arg BINARY=harness --build-arg "TARGETARCH=$DOCKER_ARCH" \
    -t sycophant-harness:local . >/dev/null
  rm "harness-linux-musl-${DOCKER_ARCH}"

  # Each toolset image carries its tools.yaml, as one-line JSON, in the
  # md.sycophant.tools label; its toolset provision command is built from that
  # label. Every value must already be a string. A non-string aborts the build.
  local ctx tag label
  for ctx in images/toolset:toolset examples/sycoadm/toolsets/hello-world:toolset-hello-world; do
    tag="${ctx#*:}"; ctx="${ctx%%:*}"
    label=$(python3 -c 'import json,sys,yaml; m=yaml.safe_load(open(sys.argv[1])); assert all(isinstance(v,str) for v in m.values()), "non-string value in "+sys.argv[1]; print(json.dumps(m,separators=(",",":")))' "$ctx/tools.yaml")
    docker build -q --build-arg "BASE_IMAGE=toolset-base:local" \
      --label "md.sycophant.tools=$label" \
      -f "$ctx/Dockerfile" "$ctx/" -t "$tag:local" >/dev/null
  done

  docker build -q --build-arg "TARGETARCH=$DOCKER_ARCH" images/kubectl/ -t sycophant-kubectl:local >/dev/null

  # provisioner bakes the component charts, so its build context is the repo root.
  docker build -q --build-arg "TARGETARCH=$DOCKER_ARCH" -f images/provisioner/Dockerfile "$REPO_ROOT" -t sycophant-provisioner:local >/dev/null
}

prepare_inference_assets() {
  # llama-server is a third-party engine: pulled by digest from trusted upstream,
  # never built here.
  # Multi-arch index -- export one arch (see import_multiarch_image).
  # Operator places this GGUF here; source: https://huggingface.co/unsloth/Qwen3.5-2B-GGUF (Qwen3.5-2B-Q4_K_M.gguf)
  local GGUF_PATH="${GGUF_PATH:-${HOME}/.cache/sycophant/weights/Qwen3.5-2B-Q4_K_M.gguf}"
  # The inference container limit must clear the mmap'd model plus its KV cache
  # and runtime, or llama.cpp OOMKills mid-load (exit 137). The model default
  # lives here and the limit is the profile's server.memory. The two must move
  # together -- assert it now so a model bump that forgets the limit fails at
  # preflight, not fifteen minutes into the run.
  if [ ! -f "$GGUF_PATH" ]; then
    printf '\n\033[1;31m✗ inference model GGUF not found: %s\033[0m\n' "$GGUF_PATH" >&2
    exit 1
  fi
  local model_mib floor_mib memory_mib
  model_mib=$(( $(wc -c < "$GGUF_PATH") / 1024 / 1024 ))
  floor_mib=$(( model_mib * 14 / 10 ))
  memory_mib="$(python3 -c 'import sys, yaml; print(yaml.safe_load(open(sys.argv[1]))["inference"]["server"]["memory"])' \
    "$REPO_ROOT/examples/sycoadm/inference/${INFERENCE_PROFILE}.yaml")"
  memory_mib="${memory_mib%Mi}"
  if [ "$memory_mib" -lt "$floor_mib" ]; then
    printf '\n\033[1;31m✗ server.memory (%dMiB) is below the floor for %s (%dMiB on disk).\n   llama.cpp mmaps the model + KV + runtime; set server.memory >= %dMi (model x 1.4) in examples/sycoadm/inference/qwen3-5-2b.yaml.\033[0m\n' \
      "$memory_mib" "$(basename "$GGUF_PATH")" "$model_mib" "$floor_mib" >&2
    exit 1
  fi
  docker build -q -f build/weights.Dockerfile \
    --build-arg "GGUF=$(basename "$GGUF_PATH")" \
    --build-arg "WEIGHTS_PATH=/weights/model.gguf" \
    -t weights:local "$(dirname "$GGUF_PATH")" >/dev/null
}

load_images_to_k3d() {
  local LLAMA_SERVER_REF="${LLAMA_SERVER_REF:-ghcr.io/ggml-org/llama.cpp:server@sha256:9f84380be42d6285a827629c809387349c3541aa8986f7536547ca33cc8dd47a}"
  step "Loading images into k3d"
  local img
  for img in inference-runtime:local \
             sycophant-harness:local relay-controller:local \
             sycophant-kubectl:local sycophant-provisioner:local \
             weights:local; do
    k3d image import "$img" --cluster "$CLUSTER_NAME" >/dev/null
  done
  # Multi-arch index -- export one arch (see import_multiarch_image).
  import_multiarch_image "$LLAMA_SERVER_REF" llama-server:local

  # Multi-arch index -- export one arch (see import_multiarch_image).
  import_multiarch_image "$RCLONE_IMAGE"
}

step_1_build() {
  step "Step 1: Build images"
  cd "$REPO_ROOT"
  build_rust_binaries
  build_go_binary
  build_docker_images
  prepare_inference_assets
  load_images_to_k3d
  ok "Images built + loaded"
}

# Seed what the tenant reads but no provision command installs: the demo
# secret the workspace grants.
seed_tenant() {
  printf '%s' "$DEMO_SECRET" | "$SYCO" tenant secret set demo-ssh-key --ns "$NAMESPACE"
  ok "Demo secret demo-ssh-key set"
}

# ---- step 3: deploy ----

deploy_tenant() {
  # The operator's path: `sycoadm tenant up -f` applies the tenant provision
  # command under the operator's Dex login and waits on its installer Job. The
  # provision runs in the background while this step seeds the namespace the
  # reactor generates.
  "$SYCO" tenant up -f "$REPO_ROOT/examples/sycoadm/tenants/hello-world.yaml" &
  local up_pid=$!
  CLEANUP_PIDS+=("$up_pid")

  wait_for "tenant namespace ${NAMESPACE} generated" 120 \
    "kubectl get namespace '$NAMESPACE' >/dev/null 2>&1"
  seed_tenant

  # The part-of label the reactor stamps triggers the tenant-rolebinding-generator,
  # which mints the per-tenant TokenReview CRB and the pod VAP binding. Kyverno
  # generate is async, so wait for the wiring before the controllers need it.
  wait_for "${NAMESPACE}-relay-tokenreview CRB" 120 \
    "kubectl get clusterrolebinding ${NAMESPACE}-relay-tokenreview >/dev/null 2>&1"
  wait_for "${NAMESPACE}-sycophant-pod-binding VAP binding" 120 \
    "kubectl get validatingadmissionpolicybinding ${NAMESPACE}-sycophant-pod-binding >/dev/null 2>&1"
  ok "Kyverno minted TokenReview CRB + pod VAP binding (label-triggered)"

  wait "$up_pid" || { warn "sycoadm tenant up failed"; exit 1; }
  ok "sycoadm tenant up: tenant provision command admitted; its installer Job succeeded (install-wait passed)"
}

deploy_inference_models() {
  # The model is its own provision command, applied once the tenant is up. The
  # harness watches the model ConfigMaps, so no restart.
  "$SYCO" inference up -f "$REPO_ROOT/examples/sycoadm/inference/${INFERENCE_PROFILE}.yaml"
  ok "sycoadm inference up: ${INFERENCE_PROFILE} model provision command installed"
}

deploy_toolsets() {
  # The toolset is its own provision command. The harness watches the toolset
  # ConfigMaps, so no restart. The example's keepalive keeps the grantless and
  # granted pods alive for the later exec assertions.
  "$SYCO" toolset up -f "$REPO_ROOT/examples/sycoadm/toolsets/hello-world/values.yaml"
  ok "sycoadm toolset up: hello-world toolset provision command installed"
}

deploy_workspace_and_user() {
  # The workspace and its bound user are one provision command each.
  "$SYCO" workspace up -f "$REPO_ROOT/examples/sycoadm/workspaces/hello-world.yaml"
  "$SYCO" instructions up -f "$REPO_ROOT/examples/sycoadm/instructions/hello-world.yaml"
  "$SYCO" user up -f "$REPO_ROOT/examples/sycoadm/users/alice.yaml" \
    --set "user.subject=${APISERVER_OIDC_ISSUER}#${TENANT_A_USER_SUB}"
  ok "sycoadm workspace up, instructions up and user up: hello-world, its instructions and its bound user installed"
}

deploy_channel() {
  # The tailnet channel is one provision command, after the base: `sycoadm
  # channel up` mints its pre-auth key from the tenant's own headscale.
  "$SYCO" channel up -f "$REPO_ROOT/examples/sycoadm/channels/tailnet.yaml"
  ok "sycoadm channel up: tailnet channel installed (headscale from tenant chart)"
  wait_for "tailnet channel Available" 180 \
    "kubectl wait --for=condition=Available --timeout=5s -n '$NAMESPACE' deploy/ch-tailnet"
  ok "Tailnet channel Available"
}

step_3_deploy() {
  step "Step 3: Deploy charts"
  deploy_tenant
  deploy_inference_models
  deploy_toolsets
  deploy_workspace_and_user
  deploy_channel
}

# ---- step 4: verify chart ----
step_4_verify() {
  step "Step 4: Verify chart"

  # The in-cluster inference server must reach Available and stay healthy: the
  # server surviving is the non-racy guarantee this run owns. It is the slowest
  # workload — the copy init container writes the multi-gigabyte GGUF into the
  # emptyDir, then the server memory-maps it on CPU — so the timeout is generous.
  kubectl wait -n "$NAMESPACE" --for=condition=Available --timeout=600s \
    "deployment/inf-${INFERENCE_PROFILE}" >/dev/null
  ok "in-cluster inference server (${INFERENCE_PROFILE}) Available"
}

# ---- step 5: app path ----
# One relay RPC over the tailnet with a person token. The request body is
# read from stdin; extra grpcurl flags follow the method.
relay_call() {
  local token="$1" method="$2"; shift 2
  # shellcheck disable=SC2086 # RELAY_GRPC_PROTO is a flag list; it must split.
  grpcurl -plaintext ${RELAY_GRPC_PROTO} -authority "$TAILNET_RELAY_ADDR" \
    -H "authorization: Bearer $token" "$@" -d @ \
    "${TAILNET_RELAY_ADDR}" "relay.v1.RelayGateway/${method}"
}

# Conversations this run creates. Step 6 inspects exactly these, so records
# left on the conversation PVC by an earlier run cannot satisfy it.
# E2E_TOOL_CONV is the ShowKernel turn, E2E_FETCH_CONV the WebFetch turn, and
# E2E_CHAT_CONV the granted GetSecret turn.
E2E_TOOL_CONV=""
E2E_FETCH_CONV=""
E2E_CHAT_CONV=""

# tool_chat <config-dir> <tool> <prompt> [chat args...]
# One syco chat turn that must call <tool>. Every tool-call approval in the
# turn is answered `y`, as a user would. Prints only the conversation ID
# syco chat reports on stderr; every log line goes to stderr.
tool_chat() {
  local dir="$1" tool="$2" prompt="$3"; shift 3
  local out err conv
  err="$(mktemp)"
  if ! out="$(SYCO_CONFIG_DIR="$dir" "$REPO_ROOT/target/syco" chat "$prompt" "$@" 2>"$err" < <(yes || true))"; then
    warn "syco chat (${tool}) exited non-zero: ${out} $(cat "$err")"
    rm -f "$err"
    return 1
  fi
  conv="$(sed -n 's/^conversation: //p' "$err" | head -1)"
  rm -f "$err"
  if ! printf '%s\n' "$out" | grep -F "⚙ ${tool} (started)" >/dev/null; then
    warn "syco chat did not call ${tool}: ${out}"
    return 1
  fi
  if [ -z "$conv" ]; then
    warn "syco chat (${tool}) did not print a conversation ID to stderr"
    return 1
  fi
  printf '%s' "$conv"
}

# The app path with the operator's person token, over the same tailnet
# forward the app uses. The relay TokenReviews the token, resolves the bound
# subject, and authorizes the named workspace by SubjectAccessReview for it.
# Drives the path through the syco CLI rather than raw grpcurl.
step_5_app_path_turn() {
  local token="$1"

  # ── isolated config dir for the operator's token ──
  local SYCO_APP_TMP
  SYCO_APP_TMP="$(mktemp -d)"
  trap 'rm -rf "$SYCO_APP_TMP"; trap - RETURN' RETURN

  envsubst < "$REPO_ROOT/examples/syco/config.toml" > "$SYCO_APP_TMP/config.toml"

  SYCO_TOKEN="$token" envsubst < "$REPO_ROOT/examples/syco/credentials.toml" > "$SYCO_APP_TMP/credentials.toml"

  # The watched identity table answers the bound list for the token's subject.
  local listed
  listed="$(SYCO_CONFIG_DIR="$SYCO_APP_TMP" "$REPO_ROOT/target/syco" whoami 2>&1)" \
    || { warn "syco whoami failed over the tailnet: ${listed}"; return 1; }
  printf '%s' "$listed" | grep -q 'hello-world' \
    || { warn "syco whoami did not list hello-world: ${listed}"; return 1; }
  ok "syco whoami answered the bound list (hello-world present)"

  # The same token on a workspace it was never granted: the review must deny.
  local denied_cfg
  denied_cfg="$(mktemp -d)"
  cat > "$denied_cfg/config.toml" <<TOML
[default]
profile = "e2e"

[profile.e2e]
server = "$TAILNET_RELAY_ADDR"
workspace = "not-granted"
TOML
  cp "$SYCO_APP_TMP/credentials.toml" "$denied_cfg/credentials.toml"

  local denied
  if denied="$(SYCO_CONFIG_DIR="$denied_cfg" "$REPO_ROOT/target/syco" chat "hello" 2>&1)"; then
    warn "an ungranted workspace was not denied over the app path: ${denied}"
    rm -rf "$denied_cfg"
    return 1
  fi
  if ! printf '%s' "$denied" | grep -qi 'permission\|denied\|PermissionDenied'; then
    warn "an ungranted workspace was not denied over the app path: ${denied}"
    rm -rf "$denied_cfg"
    return 1
  fi
  rm -rf "$denied_cfg"
  ok "Ungranted workspace denied over the app path"

  # Last, so the grantless hello-world toolset pod they start is freshly used
  # when Step 6 probes it: keepalive pods exit after 10 minutes idle.

  if ! E2E_TOOL_CONV="$(tool_chat "$SYCO_APP_TMP" ShowKernel \
      "Use the ShowKernel tool and show me its output.")"; then
    return 1
  fi
  ok "ShowKernel ran via syco chat over the app path (${E2E_TOOL_CONV})"

  if ! E2E_FETCH_CONV="$(tool_chat "$SYCO_APP_TMP" WebFetch \
      "Use the WebFetch tool to fetch https://example.com and show me the result.")"; then
    return 1
  fi
  ok "WebFetch ran via syco chat over the app path (${E2E_FETCH_CONV})"
}

# End-to-end login + chat via the Go `syco` CLI binary: write a throwaway
# config, `syco login` with the Dex credentials, bind the resulting identity
# to the hello-world workspace, then `syco chat`.
# Sets E2E_CHAT_CONV from the conversation ID syco chat prints to stderr.
step_5b_syco_cli_chat() {
  command -v go >/dev/null 2>&1 \
    || { warn "go toolchain required for the syco CLI test"; return 1; }

  # ── isolated config dir ──
  local SYCO_TMP
  SYCO_TMP="$(mktemp -d)"
  trap 'rm -rf "$SYCO_TMP"; trap - RETURN' RETURN

  envsubst < "$REPO_ROOT/examples/syco/config.toml" > "$SYCO_TMP/config.toml"

  # ── login ──
  SYCO_CONFIG_DIR="$SYCO_TMP" SYCO_PASSWORD="$SYCO_PASSWORD" \
    "$REPO_ROOT/target/syco" login --email "$SYCO_EMAIL" \
    || { warn "syco login failed"; return 1; }
  ok "syco login succeeded"

  # ── extract sub claim from the saved token ──
  local raw_token sub
  raw_token="$(awk -F'"' '/token/ {print $2}' "$SYCO_TMP/credentials.toml")"
  [ -n "$raw_token" ] || { warn "no token found in credentials.toml"; return 1; }

  sub="$(jwt_claim "$raw_token" sub)"
  [ -n "$sub" ] && [ "$sub" != "null" ] \
    || { warn "could not decode sub from the saved JWT"; return 1; }
  ok "Decoded sub from JWT: ${sub}"

  # ── bind identity to workspace ──
  "$SYCO" user up -f "$REPO_ROOT/examples/sycoadm/users/alice.yaml" \
    --set "user.subject=${APISERVER_OIDC_ISSUER}#${sub}" \
    --set "user.workspaces={hello-world}" \
    || { warn "sycoadm user up failed for the syco identity"; return 1; }
  ok "Bound ${APISERVER_OIDC_ISSUER}#${sub} to hello-world"

  # ── chat ──
  # The granted turn: GetSecret reads demo-ssh-key through the demo-key grant.
  if ! E2E_CHAT_CONV="$(tool_chat "$SYCO_TMP" GetSecret \
      "Use the GetSecret tool to read the secret named demo-ssh-key." \
      --grant hello-world/demo-key)"; then
    return 1
  fi
  ok "syco chat completed with a GetSecret tool call (${E2E_CHAT_CONV})"

  # ── conversation-ID resume ──
  # Every leg runs from its own config dir under SYCO_TMP holding copies of
  # this profile's config and credentials, so the RETURN trap cleans them all.
  local short_prompt="Reply with one short sentence. Do not use any tools."
  # leg_config_dir <name>: prints the path of a fresh leg config dir.
  leg_config_dir() {
    local d="$SYCO_TMP/$1"
    mkdir -p "$d"
    cp "$SYCO_TMP/config.toml" "$SYCO_TMP/credentials.toml" "$d/"
    printf '%s' "$d"
  }

  # leftover_config_dir <name> <conversation-id>: a leg config dir left over
  # from an earlier cluster: a saved message number of 727 and a newest
  # history line for hello-world naming <conversation-id>.
  leftover_config_dir() {
    local d
    d="$(leg_config_dir "$1")"
    mkdir -p "$d/cursors"
    printf '727\n' > "$d/cursors/hello-world.seq"
    printf '%s\thello-world\t%s\n' "$2" "$(date -u +%Y-%m-%dT%H:%M:%SZ)" > "$d/history"
    printf '%s' "$d"
  }

  # bounded_chat <config-dir> <bound-seconds> <stdout-file> <stderr-file> <chat args...>
  # Runs a one-shot in the background and SIGKILLs it once the bound passes.
  # Returns the one-shot's exit status, or 124 when it was killed at the bound.
  bounded_chat() {
    local dir="$1" bound="$2" out="$3" err="$4"; shift 4
    SYCO_CONFIG_DIR="$dir" "$REPO_ROOT/target/syco" chat "$@" < <(yes || true) >"$out" 2>"$err" &
    local pid=$! rc=0
    if ! wait_for "one-shot to exit" "$bound" "! kill -0 $pid 2>/dev/null"; then
      kill -9 "$pid" 2>/dev/null || true
      wait "$pid" 2>/dev/null || true
      return 124
    fi
    wait "$pid" || rc=$?
    return "$rc"
  }

  local leg_rc

  # Leftover config, no --continue/--resume: a new conversation still answers.
  local lo_new_dir
  lo_new_dir="$(leftover_config_dir leftover-new "$(python3 -c 'import uuid; print(uuid.uuid4())')")"
  leg_rc=0
  bounded_chat "$lo_new_dir" 300 "$lo_new_dir/out" "$lo_new_dir/err" \
    "$short_prompt" || leg_rc=$?
  if [ "$leg_rc" -ne 0 ]; then
    warn "one-shot from a leftover config did not exit 0 within 300s (rc=${leg_rc}): $(cat "$lo_new_dir/err")"
    return 1
  fi
  if [ ! -s "$lo_new_dir/out" ]; then
    warn "one-shot from a leftover config exited 0 with empty stdout: $(cat "$lo_new_dir/err")"
    return 1
  fi
  ok "One-shot from a leftover config exited 0 with non-empty stdout within 300s"

  # Leftover config with --continue: refused, naming the unknown ID.
  local lo_cont_dir lo_unknown_id
  lo_unknown_id="$(python3 -c 'import uuid; print(uuid.uuid4())')"
  lo_cont_dir="$(leftover_config_dir leftover-continue "$lo_unknown_id")"
  leg_rc=0
  bounded_chat "$lo_cont_dir" 300 "$lo_cont_dir/out" "$lo_cont_dir/err" \
    --continue "$short_prompt" || leg_rc=$?
  if [ "$leg_rc" -eq 0 ] || [ "$leg_rc" -eq 124 ]; then
    warn "one-shot --continue to an unknown ID did not exit non-zero within 300s (rc=${leg_rc}): $(cat "$lo_cont_dir/err")"
    return 1
  fi
  if ! grep -qF "$lo_unknown_id" "$lo_cont_dir/err"; then
    warn "one-shot --continue to an unknown ID exited non-zero without naming ${lo_unknown_id} on stderr: $(cat "$lo_cont_dir/err")"
    return 1
  fi
  ok "One-shot --continue from a leftover config exited non-zero with the unknown conversation ID on stderr"

  # Scenario K: kill a running one-shot as soon as its conversation ID is saved.
  local k_dir k_pid k_id k_msg
  k_msg="Write a 400-word essay on the history of lighthouses. Do not use any tools."
  k_dir="$(leg_config_dir scenario-k)"
  k_saved() {
    [ -f "$k_dir/history" ] && awk -F'\t' '$2 == "hello-world" { found = 1 } END { exit !found }' "$k_dir/history"
  }
  k_saved_or_exited() { k_saved || ! kill -0 "$k_pid" 2>/dev/null; }
  SYCO_CONFIG_DIR="$k_dir" "$REPO_ROOT/target/syco" chat "$k_msg" \
    < <(yes || true) >"$k_dir/out" 2>"$k_dir/err" &
  k_pid=$!
  if ! wait_for "a hello-world history line from the running one-shot" 300 k_saved_or_exited; then
    kill -9 "$k_pid" 2>/dev/null || true
    wait "$k_pid" 2>/dev/null || true
    return 1
  fi
  kill -9 "$k_pid" 2>/dev/null || true
  wait "$k_pid" 2>/dev/null || true
  if ! k_saved; then
    warn "one-shot exited without saving a hello-world history line: $(cat "$k_dir/err")"
    return 1
  fi
  if grep -q '^conversation: ' "$k_dir/err"; then
    warn "one-shot finished its turn before its conversation ID was saved; scenario K needs the turn still running"
    return 1
  fi
  k_id="$(awk -F'\t' '$2 == "hello-world" { print $1; exit }' "$k_dir/history")"

  # k_history: GetConversationHistory for K's ID, as grpcurl JSON.
  k_history() {
    jq -nc --arg c "$k_id" '{conversationId: $c, workspace: "hello-world"}' \
      | relay_call "$raw_token" GetConversationHistory 2>/dev/null
  }
  # k_entries: the history as [{r: role, t: concatenated text}].
  local k_entries='[.entries[] | {r: .message.role, t: ([.message.content[]?.text.text // empty] | join(""))}]'
  k_user_present() {
    k_history | jq -e --arg m "$k_msg" "${k_entries} | any(.r == \"user\" and .t == \$m)" >/dev/null
  }
  k_reply_present() {
    k_history | jq -e --arg m "$k_msg" "${k_entries} as \$e
      | ([range(0; \$e | length) | select(\$e[.].r == \"user\" and \$e[.].t == \$m)] | first) as \$u
      | if \$u == null then false
        else any(\$e[\$u + 1:][]; .r == \"assistant\" and (.t | length) > 0) end" >/dev/null
  }
  if ! wait_for "the killed one-shot's user entry in server history" 30 k_user_present; then
    warn "GetConversationHistory for ${k_id} holds no user entry with the killed one-shot's message: $(k_history)"
    return 1
  fi
  ok "Server history for the killed one-shot's conversation holds its user message"
  if ! wait_for "an assistant reply after the killed one-shot's message" 300 k_reply_present; then
    warn "GetConversationHistory for ${k_id}: $(k_history)"
    return 1
  fi
  ok "Server history for the killed one-shot's conversation gained a non-empty assistant entry after its message"

  # --continue from K's config dir resumes the killed conversation.
  leg_rc=0
  bounded_chat "$k_dir" 300 "$k_dir/continue.out" "$k_dir/continue.err" \
    --continue "$short_prompt" || leg_rc=$?
  if [ "$leg_rc" -ne 0 ]; then
    warn "one-shot --continue after the killed one-shot did not exit 0 (rc=${leg_rc}): $(cat "$k_dir/continue.err")"
    return 1
  fi
  if [ ! -s "$k_dir/continue.out" ]; then
    warn "one-shot --continue after the killed one-shot exited 0 with empty stdout: $(cat "$k_dir/continue.err")"
    return 1
  fi
  ok "One-shot --continue into the killed one-shot's conversation exited 0 with non-empty stdout"
}

# Join this host to this run's tailnet.
tailnet_join_macos() {
  # Sentinel first, then the banner, then the prompt — so the monitor fires
  # before sudo blocks on input.
  : > "$SUDO_PENDING_SENTINEL"
  printf '\n\033[1;33m🔑 SUDO INCOMING: the tailnet join needs your macOS password next.\033[0m\n' >&2
  local rc=0
  "$SYCO" connect --tenant hello-world || rc=$?
  rm -f "$SUDO_PENDING_SENTINEL"
  if [ "$rc" -ne 0 ]; then
    warn "sycoadm connect failed (rc=$rc); join manually and retry"
    return 1
  fi
  ok "Host joined the tailnet (sycoadm connect, re-registered against this run's headscale)"
}

step_5_channel() {
  step "Step 5: App path"

  # The app path is reached over the fresh tailnet, so this host joins it
  # first on every run.
  tailnet_join_macos || return 1

  # The operator's person token: a Dex-signed ID token for the subject
  # `sycoadm user up` bound to the workspace list.
  local token
  token="$(mint_dex_token "$TENANT_A_USER_SUB")"
  step_5_app_path_turn "$token" \
    || { warn "the operator-token app-path turn failed"; return 1; }
  ok "App path proven with the operator token (denial, chat turn, tool call)"

  step_5b_syco_cli_chat \
    || { warn "syco CLI login + chat test failed"; return 1; }
  ok "syco CLI login + chat proven end-to-end"
}

# ---- step 6: security assertions ----
# attach_ephemeral_scrubber <namespace> <pod> <container-name>: attach an
# ephemeral busybox to <pod>'s harness container, sharing its PID namespace.
attach_ephemeral_scrubber() {
  local ns="$1" pod="$2" container_name="$3"
  local patch='{"spec":{"ephemeralContainers":[{"name":"'"$container_name"'","image":"busybox:1.36","command":["sleep","180"],"targetContainerName":"harness","securityContext":{"runAsNonRoot":true,"runAsUser":1000,"readOnlyRootFilesystem":true,"allowPrivilegeEscalation":false,"capabilities":{"drop":["ALL"]},"seccompProfile":{"type":"RuntimeDefault"}}}]}}'
  kubectl patch pod "$pod" -n "$ns" \
    --subresource=ephemeralcontainers --type=strategic -p "$patch" >/dev/null
}

discover_conversation_dirs() {
  local tb_pod="$1" scrub_c="$2" conv_root="$3"
  # Every one of this run's conversations must be on the PVC before any scan
  # of it counts: an empty directory would pass every absence check below.
  local chat_dir="" tool_dir="" fetch_dir=""
  for _ in $(seq 1 15); do
    chat_dir="$(kubectl exec -n "$NAMESPACE" "$tb_pod" -c "$scrub_c" -- \
      find "$conv_root" -maxdepth 2 -type d -name "$E2E_CHAT_CONV" 2>/dev/null | head -1)" || true
    tool_dir="$(kubectl exec -n "$NAMESPACE" "$tb_pod" -c "$scrub_c" -- \
      find "$conv_root" -maxdepth 2 -type d -name "$E2E_TOOL_CONV" 2>/dev/null | head -1)" || true
    fetch_dir="$(kubectl exec -n "$NAMESPACE" "$tb_pod" -c "$scrub_c" -- \
      find "$conv_root" -maxdepth 2 -type d -name "$E2E_FETCH_CONV" 2>/dev/null | head -1)" || true
    # The ephemeral container may not be running yet; under set -e the
    # assignments above are guarded so this loop can retry.
    if [ -n "$chat_dir" ] && [ -n "$tool_dir" ] && [ -n "$fetch_dir" ]; then
      break
    fi
    sleep 2
  done
  if [ -z "$chat_dir" ] || [ -z "$tool_dir" ] || [ -z "$fetch_dir" ]; then
    warn "this run's conversations are not on the PVC (chat=${chat_dir:-none} tool=${tool_dir:-none} fetch=${fetch_dir:-none})"
    return 1
  fi
  printf '%s\n%s\n%s\n' "$chat_dir" "$tool_dir" "$fetch_dir"
}

# tool_result_texts <log-file> <tool>: a JSON array of the text of every <tool>
# result in the conversation log at <log-file> (one JSON record per line).
tool_result_texts() {
  jq -s --arg t "$2" '
    [.[] | select(.role == "assistant") | .tool_calls[]? | select(.name == $t) | .id] as $ids
    | [.[] | select(.role == "tool" and ((.tool_call_id // "") as $i | $ids | index($i)) != null)
           | [.content[]?.block.Text.text // empty] | join("")]' "$1"
}

# conversation_has_result <tb_pod> <scrub_c> <conv-dir> <tool> <jq-test> <log-file>
# Copies the conversation log out to <log-file> and holds when some <tool>
# result passes <jq-test> (a jq filter over the result text). wait_for polls it:
# the harness appends the log as the turn runs.
conversation_has_result() {
  local tb_pod="$1" scrub_c="$2" dir="$3" tool="$4" test="$5" log="$6"
  kubectl exec -n "$NAMESPACE" "$tb_pod" -c "$scrub_c" -- \
    cat "$dir/conversation.json" > "$log" 2>/dev/null || return 1
  tool_result_texts "$log" "$tool" | jq -e "any(.[]; $test)" >/dev/null
}

assert_tool_results() {
  local tb_pod="$1" scrub_c="$2" tool_dir="$3" fetch_dir="$4"
  local log; log="$(mktemp)"
  trap 'rm -f "'"$log"'"; trap - RETURN' RETURN

  # The grantless toolset pod runs on gVisor: its kernel log opens with the
  # gVisor banner.
  if wait_for "ShowKernel result holds the gVisor banner" 60 \
       "conversation_has_result '$tb_pod' '$scrub_c' '$tool_dir' ShowKernel 'contains(\"Starting gVisor\")' '$log'"; then
    ok "ShowKernel result reports a gVisor kernel"
  else
    warn "ShowKernel result did not hold 'Starting gVisor': $(tool_result_texts "$log" ShowKernel 2>/dev/null || true)"
    return 1
  fi

  # The grantless toolset has no egress hole: the fetch never gets a response.
  if wait_for "WebFetch result is a network error" 60 \
       "conversation_has_result '$tb_pod' '$scrub_c' '$fetch_dir' WebFetch 'startswith(\"WebFetch: network error:\")' '$log'"; then
    ok "WebFetch of https://example.com failed with a network error (egress closed)"
  else
    warn "WebFetch result did not start with 'WebFetch: network error:': $(tool_result_texts "$log" WebFetch 2>/dev/null || true)"
    return 1
  fi
}

assert_scrubbing() {
  local tb_pod="$1" scrub_c="$2" conv_root="$3" chat_dir="$4"
  # The granted demo secret: the redaction marker must appear in this run's
  # GetSecret turn and the raw value nowhere under the conversation root —
  # together proving the granted tool call read the secret end-to-end and the
  # value was scrubbed on the way out of the pod.
  if wait_for "redaction marker in this run's GetSecret turn" 60 \
       "kubectl exec -n '$NAMESPACE' '$tb_pod' -c '$scrub_c' -- grep -rlF '[REDACTED:demo-ssh-key]' '$chat_dir' >/dev/null 2>&1"; then
    ok "GetSecret turn holds [REDACTED:demo-ssh-key]"
  else
    warn "this run's GetSecret turn holds no [REDACTED:demo-ssh-key] marker"
    return 1
  fi

  # grep -l exits 1 when no file matches; the scan reports its own status so a
  # failed scan cannot pass as "no match".
  local scan
  # shellcheck disable=SC2016 # single-quoted on purpose: the pod's sh expands it.
  scan="$(kubectl exec -n "$NAMESPACE" "$tb_pod" -c "$scrub_c" -- \
    sh -c 'grep -rlF "$1" "$2"; echo "rc=$?"' sh "$DEMO_SECRET" "$conv_root" 2>/dev/null || true)"
  if [ "$(printf '%s\n' "$scan" | tail -1)" = "rc=1" ]; then
    ok "The demo secret value is in no file under the conversation root"
  else
    warn "demo secret scan of the conversation root failed or found files: ${scan:-no output}"
    return 1
  fi
}

assert_credential_isolation() {
  local task_pod="$1"
  # Positive control first: the granted pod holds the demo key at the grant's
  # path, so a broken grant mount cannot pass the absence checks vacuously.
  local granted_selector="app.kubernetes.io/component=capability-job,sycophant.md/workspace=hello-world,sycophant.md/toolset=hello-world,sycophant.md/grant=demo-key"
  local granted_pod
  if ! wait_for "granted hello-world toolset pod for demo-key" 90 \
       "kubectl get pod -n '$NAMESPACE' -l '$granted_selector' -o name 2>/dev/null | grep . >/dev/null"; then
    return 1
  fi
  granted_pod="$(kubectl get pod -n "$NAMESPACE" -l "$granted_selector" \
    -o jsonpath='{.items[0].metadata.name}')"
  if ! kubectl exec -n "$NAMESPACE" "$granted_pod" -- test -f /home/agent/.ssh/id_ed25519 >/dev/null 2>&1; then
    warn "granted pod has no demo key at /home/agent/.ssh/id_ed25519 (${granted_pod})"
    return 1
  fi
  ok "Granted pod holds the demo key (${granted_pod})"

  # The grantless pod resolved no grant: neither the grant's path nor the
  # convention credential target may exist in it.
  assert_path_absent "$task_pod" "$NAMESPACE" /home/agent/.ssh/id_ed25519 "grant leak"
  assert_path_absent "$task_pod" "$NAMESPACE" /run/secrets/grant/credential "credential leak"
  ok "Credential isolation (no grant credential in the grantless toolset pod)"
}

dispatch_security_substeps() {
  # Each substep is an independent security assertion sharing no state. Run
  # them all and fail at the end if any failed, so one failure never aborts the
  # rest and masks unrelated properties.
  local -a checks=(
    step_6_relay_sheds_tsnet
    step_6_inference_served
    step_6_identity_hot_reload
  )
  local failed=() c
  for c in "${checks[@]}"; do
    "$c" || failed+=("$c")
  done
  if [ "${#failed[@]}" -ne 0 ]; then
    warn "Step 6 substeps failed: ${failed[*]}"
    return 1
  fi
}

step_6_security() {
  step "Step 6: Security assertions"

  if [ -z "$E2E_TOOL_CONV" ] || [ -z "$E2E_FETCH_CONV" ] || [ -z "$E2E_CHAT_CONV" ]; then
    warn "Step 5 recorded no conversations to inspect"
    return 1
  fi

  # The harness creates the grantless hello-world toolset pod on the first
  # grantless call; 90s covers the ARM64 gVisor cold-start slow path.
  local toolset_selector="app.kubernetes.io/component=capability-job,sycophant.md/workspace=hello-world,sycophant.md/toolset=hello-world,!sycophant.md/grant"
  local task_pod
  wait_for "grantless hello-world toolset pod" 90 \
    "kubectl get pod -n '$NAMESPACE' -l '$toolset_selector' -o name 2>/dev/null | grep -q ."
  task_pod="$(kubectl get pod -n "$NAMESPACE" \
                -l "$toolset_selector" \
                -o jsonpath='{.items[0].metadata.name}')"

  kubectl wait -n "$NAMESPACE" --for=condition=Ready --timeout=60s "pod/$task_pod" >/dev/null
  ok "hello-world toolset pod Ready ($task_pod)"

  # The conversation log is on the harness's OWN RWO PVC
  # (ws-<ws>-conversation at /var/lib/harness/conversations). A separate
  # pod can't mount an RWO PVC, and the harness image is FROM scratch (no
  # shell), so attach an ephemeral busybox to the harness pod sharing its
  # PID namespace and read the dir via /proc/1/root.
  local tb_pod scrub_c
  tb_pod="$(kubectl get pod -n "$NAMESPACE" \
    -l app.kubernetes.io/component=harness,sycophant.md/workspace=hello-world \
    -o jsonpath='{.items[0].metadata.name}')"
  scrub_c="syco-scrub-$$"
  attach_ephemeral_scrubber "$NAMESPACE" "$tb_pod" "$scrub_c"

  local conv_root=/proc/1/root/var/lib/harness/conversations
  local dirs chat_dir tool_dir fetch_dir
  dirs="$(discover_conversation_dirs "$tb_pod" "$scrub_c" "$conv_root")"
  { read -r chat_dir; read -r tool_dir; read -r fetch_dir; } <<< "$dirs"

  local event_logs
  if ! event_logs="$(kubectl exec -n "$NAMESPACE" "$tb_pod" -c "$scrub_c" -- \
      find "$conv_root" -name event_log.jsonl)"; then
    warn "could not scan the conversation volume for event_log.jsonl"
    return 1
  fi
  if [ -n "$event_logs" ]; then
    warn "the conversation volume holds event_log.jsonl files: ${event_logs}"
    return 1
  fi
  ok "The conversation volume holds no event_log.jsonl file"

  assert_tool_results "$tb_pod" "$scrub_c" "$tool_dir" "$fetch_dir"
  assert_scrubbing "$tb_pod" "$scrub_c" "$conv_root" "$chat_dir"
  assert_credential_isolation "$task_pod"
  dispatch_security_substeps
}

# The tailnet terminus lives on the tailnet channel, not the relay. A tailscale
# container back in the relay pod would widen the relay's per-pod egress and
# let loopback bypass both the tailnet and the CNP.
step_6_relay_sheds_tsnet() {
  local images
  images="$(kubectl get deploy relay-controller -n "$NAMESPACE" \
    -o jsonpath='{range .spec.template.spec.containers[*]}{.name}={.image}{"\n"}{end}')"
  if printf '%s' "$images" | grep -qi 'tailscale\|tsnet'; then
    warn "relay pod carries a tailscale/tsnet container: $images"
    return 1
  fi
  ok "Relay pod carries no tailscale container"
}

# From a harness-labelled pod, a completion to the in-cluster inference Service
# returns 200. The probe dials the cluster Service by name, so this proves a turn
# reaches the in-cluster server rather than an external provider.
step_6_inference_served() {
  step "In-cluster inference (${INFERENCE_PROFILE})"
  local svc="inf-${INFERENCE_PROFILE}.${NAMESPACE}.svc.cluster.local"
  # The pod carries the harness label, so ws-<ws>-egress grants it egress to the
  # inference endpoint and DNS for the Service FQDN.
  kubectl delete pod inference-l7-probe -n "$NAMESPACE" --ignore-not-found --wait=true >/dev/null 2>&1
  export svc INFERENCE_PORT INFERENCE_PROFILE
  envsubst '${svc} ${INFERENCE_PORT} ${INFERENCE_PROFILE}' \
    < "$REPO_ROOT/scripts/fixtures/inference-l7-probe-pod.yaml.tpl" \
    | kubectl apply -n "$NAMESPACE" -f - >/dev/null
  if ! kubectl wait --for=jsonpath='{.status.phase}'=Succeeded pod/inference-l7-probe \
         -n "$NAMESPACE" --timeout=120s >/dev/null 2>&1; then
    warn "inference probe did not complete"
    kubectl logs inference-l7-probe -n "$NAMESPACE" 2>&1 | tail -10 || true
    kubectl delete pod inference-l7-probe -n "$NAMESPACE" --ignore-not-found >/dev/null 2>&1
    return 1
  fi
  local out completion_code toolcall_code toolcalls
  out="$(kubectl logs inference-l7-probe -n "$NAMESPACE" 2>/dev/null)"
  kubectl delete pod inference-l7-probe -n "$NAMESPACE" --ignore-not-found >/dev/null 2>&1
  completion_code="$(printf '%s' "$out" | sed -n 's/^COMPLETION=//p')"
  toolcall_code="$(printf '%s' "$out" | sed -n 's/^TOOLCALL=//p')"
  toolcalls="$(printf '%s' "$out" | sed -n 's/^TOOLCALLS=//p')"

  if [ "$completion_code" = "200" ]; then
    ok "In-cluster turn served: completion returned 200 from the inference pod (not an external provider)"
  else
    warn "completion did not return 200 from the inference Service: got ${completion_code:-none}"
    return 1
  fi

  # A tools request that must trigger a call.
  if [ "$toolcall_code" = "200" ] && [ "$toolcalls" = "present" ]; then
    ok "Tool-call turn served: the model returned a non-empty tool_calls with 200 (the chat template renders the tools)"
  else
    warn "tool-call turn failed: status=${toolcall_code:-none} tool_calls=${toolcalls:-none} — a broken or dropped chat template drops the tools array (empty/null tool_calls); a missing template file crashes the server (status != 200)"
    return 1
  fi
}

# Identity is live operator data: the operator re-runs `sycoadm user up` and the
# relay's watched table answers the change without a pod restart. The probe
# binds a second subject, waits for its identity ConfigMap to appear in the
# watched answer, then removes the binding and waits for the answer to go
# empty — the whole revocation promise on the ListWorkspaces surface.
step_6_identity_hot_reload() {
  step "Identity hot reload"
  local before after token listed
  local probe_sub="e2e-hot-reload"

  before="$(kubectl get pod -n "$NAMESPACE" -l app.kubernetes.io/component=relay-controller \
    -o jsonpath='{.items[0].metadata.name}')"

  # Bind the probe subject to the workspace, then wait for the relay to
  # answer its list.
  # Its own handle: a handle two subjects claim resolves to neither, which
  # would unbind the main user's slack handle for the length of the probe.
  "$SYCO" user up -f "$REPO_ROOT/examples/sycoadm/users/alice.yaml" \
    --set "user.subject=${APISERVER_OIDC_ISSUER}#${probe_sub}" \
    --set "user.handles.slack=E2E-HOT-RELOAD" \
    >/dev/null \
    || { warn "sycoadm user up failed for the hot-reload probe"; return 1; }

  token="$(mint_dex_token "$probe_sub")"
  # shellcheck disable=SC2016 # single-quoted on purpose: wait_for evals it.
  if wait_for "relay answers the freshly bound subject's list" 90 \
       'relay_call "'"$token"'" ListWorkspaces <<<"{}" 2>/dev/null | grep -q hello-world'; then
    ok "Freshly bound subject reaches the workspace without a restart"
  else
    warn "relay never answered the freshly bound subject's list"
    return 1
  fi

  # Remove the binding the operator's way, `sycoadm user down`: the user's
  # release goes, and the identity ConfigMap and the reach RoleBindings with
  # it. The relay's next answer must be empty.
  local probe_subject="${APISERVER_OIDC_ISSUER}#${probe_sub}" release_name
  release_name="user-$(handle_of "$probe_subject")"
  # The provision installers keep release records in ConfigMaps.
  if ! HELM_DRIVER=configmap helm status "$release_name" -n "$NAMESPACE" >/dev/null 2>&1; then
    warn "the probe user's release ${release_name} is not installed"
    return 1
  fi
  if ! "$SYCO" user down "$probe_subject" --tenant hello-world; then
    warn "sycoadm user down failed for the hot-reload probe"
    return 1
  fi
  if HELM_DRIVER=configmap helm status "$release_name" -n "$NAMESPACE" >/dev/null 2>&1; then
    warn "the probe user's release ${release_name} is still installed after sycoadm user down"
    return 1
  fi
  ok "sycoadm user down removed the probe user's release ${release_name}"
  # A dropped tailnet or a crashed relay must not count as revoked: the
  # answer must be a successful list without hello-world, or a denial.
  # shellcheck disable=SC2016 # single-quoted on purpose: wait_for evals it.
  if wait_for "revoked subject's list goes empty" 90 \
       'out="$(relay_call "'"$token"'" ListWorkspaces <<<"{}" 2>&1)"; rc=$?;
        { [ "$rc" -eq 0 ] && ! printf "%s" "$out" | grep -q hello-world; } \
          || printf "%s" "$out" | grep -qE "PermissionDenied|Unauthenticated"'; then
    ok "Binding removal revoked the subject without a restart"
  else
    warn "relay kept answering the revoked subject's list — revocation did not land"
    return 1
  fi

  after="$(kubectl get pod -n "$NAMESPACE" -l app.kubernetes.io/component=relay-controller \
    -o jsonpath='{.items[0].metadata.name}')"
  if [ "$before" != "$after" ]; then
    warn "relay pod restarted during the hot-reload probe ($before -> $after)"
    return 1
  fi
  ok "Identity hot reload completed with no relay restart"
}

# ---- step 7: sycoadm upgrade via the CLI ----
# Exercises the operator upgrade path: it re-extracts the binary's charts and
# re-provisions the main tenant from its provision command, so this is the only
# coverage that `sycoadm upgrade` actually delivers the binary's charts. Wiping the config-root charts first is the regression guard: an upgrade
# that skips the chart re-extract has nothing to apply and fails here.
step_7_upgrade_cli() {
  step "Step 7: sycoadm upgrade (CLI chart delivery)"

  cargo build --release -p sycoadm >/dev/null
  local syco="$REPO_ROOT/target/release/sycoadm"

  rm -rf "${SYCO_CONFIG_DIR}/charts"

  if "$syco" upgrade; then
    ok "sycoadm upgrade succeeded (re-extracted charts + applied cluster + tenant)"
  else
    warn "sycoadm upgrade FAILED — CLI did not deliver its embedded charts"
    return 1
  fi

  if [ -f "${SYCO_CONFIG_DIR}/charts/sycophant-cluster/Chart.yaml" ]; then
    ok "config-root charts re-extracted from the binary"
  else
    warn "sycoadm upgrade left the config-root charts missing"
    return 1
  fi

  # Re-provisioning keeps the tenant's own settings, so headscale stays up.
  if kubectl rollout status deploy/headscale -n "$NAMESPACE" --timeout=120s >/dev/null 2>&1; then
    ok "headscale still running in ${NAMESPACE} after the upgrade"
  else
    warn "sycoadm upgrade removed headscale from ${NAMESPACE}"
    return 1
  fi
}

# Apiserver-trusted issuer for the per-identity cluster-identity proof: the
# bundled Dex. The apiserver only accepts an https issuer, and k3s runs
# kube-apiserver in the node netns with no CoreDNS, so Dex is published on a
# pinned NodePort the apiserver reaches at the node loopback. The self-signed
# serving cert and the rendered AuthenticationConfiguration are generated at
# boot into tmp/oidc-apiserver and mounted into the apiserver; the same cert is
# handed to Dex as its TLS listener, so both ends share one CA.
readonly APISERVER_OIDC_NS="sycophant-system"
readonly APISERVER_OIDC_NODEPORT=30443
readonly APISERVER_OIDC_ISSUER="https://127.0.0.1:${APISERVER_OIDC_NODEPORT}/dex"
readonly APISERVER_OIDC_AUDIENCE="sycophant-cluster"
# The identity sub each token carries; the apiserver prefixes it with the
# issuer to form the RoleBinding subject <issuer>#<sub>.
readonly TENANT_A_USER_SUB="tenant-a-user"

# Command contract. Command names and the label mirror the cluster
# chart's reactor policies. The reactor prefixes the bare tenant token with
# tenant-. The lifecycle tenant is owned by an issuer-prefixed operator identity.
readonly LIFECYCLE_OPERATOR_SUB="operator-provisioner"
readonly LIFECYCLE_TENANT_TOKEN="lifecycle-probe"
readonly LIFECYCLE_TENANT_NS="tenant-${LIFECYCLE_TENANT_TOKEN}"
readonly LIFECYCLE_WORKSPACE="hello-world"
# A toolset name absent from the base install, so its ConfigMap can only land
# via its provision command.
readonly LIFECYCLE_TOOLSET="lifecycle-tool"

# ---- sycoadm inputs ----
# e2e drives the operator's real path through `sycoadm`. Each SYCO_* input below
# defaults for e2e and may be overridden from the environment.
readonly SYCO="$REPO_ROOT/target/release/sycoadm"
# sycoadm's config root for this run, so e2e never touches the operator's own
# ~/.config/sycophant. `sycoadm setup` regenerates the Dex trust under it.
export SYCO_CONFIG_DIR="${SYCO_CONFIG_DIR:-$REPO_ROOT/tmp/syco-config}"
# Dex's operator login. The password persists for reuse runs against the same
# Dex and is regenerated with the cluster.
readonly SYCO_PASSWORD_FILE="$REPO_ROOT/tmp/e2e-operator-password"
# The public Dex CA `sycoadm setup` generated and the apiserver trusts.
readonly SYCO_DEX_CA="$SYCO_CONFIG_DIR/oidc/ca.crt"
export SYCO_EMAIL="${SYCO_EMAIL:-e2e-operator@sycophant.local}"
# Step 1 builds and loads the e2e image set itself.
export SYCO_SKIP_BUILD="${SYCO_SKIP_BUILD:-1}"
# The Cilium agent image comes from the host Docker cache, not a run-time pull.
export SYCO_CILIUM_PRELOAD="${SYCO_CILIUM_PRELOAD:-1}"
export SYCO_KYVERNO_VALUES="${SYCO_KYVERNO_VALUES:-$REPO_ROOT/scripts/fixtures/kyverno-values.yaml}"
export SYCO_CLUSTER_VALUES="${SYCO_CLUSTER_VALUES:-$REPO_ROOT/scripts/fixtures/cluster-values.yaml}"

# Export SYCO_PASSWORD: the caller's, else the persisted one, else a new one.
syco_credentials() {
  if [ -z "${SYCO_PASSWORD:-}" ]; then
    if [ ! -s "$SYCO_PASSWORD_FILE" ]; then
      mkdir -p "$(dirname "$SYCO_PASSWORD_FILE")"
      ( umask 077; head -c 256 /dev/urandom | LC_ALL=C tr -dc 'a-zA-Z0-9' | cut -c1-24 > "$SYCO_PASSWORD_FILE" )
    fi
    SYCO_PASSWORD="$(cat "$SYCO_PASSWORD_FILE")"
  fi
  export SYCO_PASSWORD
}

# Build a kubeconfig at $1 whose sole context (named $2) presents bearer token
# $3 to the current apiserver with its CA and no client cert. kubectl prefers a
# client cert over a token, so reusing the admin context would skip the OIDC path.
kubeconfig_with_token() {
  local kc="$1" name="$2" token="$3" server
  server="$(kubectl config view --minify -o jsonpath='{.clusters[0].cluster.server}')"
  kubectl config view --minify --raw \
    -o jsonpath='{.clusters[0].cluster.certificate-authority-data}' \
    | openssl base64 -d -A > "$kc.ca"
  [ -s "$kc.ca" ] || { warn "could not read the apiserver CA from the current kubeconfig"; exit 1; }
  KUBECONFIG="$kc" kubectl config set-cluster e2e \
    --server="$server" --certificate-authority="$kc.ca" --embed-certs=true >/dev/null
  KUBECONFIG="$kc" kubectl config set-credentials "$name" --token="$token" >/dev/null
  KUBECONFIG="$kc" kubectl config set-context "$name" --cluster=e2e --user="$name" >/dev/null
  KUBECONFIG="$kc" kubectl config use-context "$name" >/dev/null
}

# Sign a JWT with Dex's active signing key so it validates against Dex's JWKS.
# Reads the private JWK from the openid-connect-keys SigningKey singleton Dex's
# storage created, and emits a compact token carrying Dex's issuer, the
# apiserver audience, and the given sub. Identity is the sub alone; no groups
# claim is set.
mint_dex_token() {
  local sub="$1"
  kubectl get signingkeies openid-connect-keys -n "$APISERVER_OIDC_NS" -o json \
    | ISS="$APISERVER_OIDC_ISSUER" AUD="$APISERVER_OIDC_AUDIENCE" SUB="$sub" python3 -c '
import json, os, sys, time
import jwt
from jwt.algorithms import RSAAlgorithm
sk = json.load(sys.stdin)["signingKey"]
key = RSAAlgorithm.from_jwk(json.dumps(sk))
now = int(time.time())
sys.stdout.write(jwt.encode(
    {"iss": os.environ["ISS"], "sub": os.environ["SUB"],
     "aud": os.environ["AUD"], "iat": now, "exp": now + 600},
    key, algorithm="RS256", headers={"kid": sk["kid"]}))
'
}

# ---- per-identity cluster-identity isolation proof ----
# Live proof that a Dex token authenticates as the user's issuer-prefixed
# identity, that a system:-prefixed sub is refused authentication, and that
# every binding naming the user comes from the user provision command. Uses the
# bundled Dex on the node-reachable NodePort the apiserver already trusts (its
# TLS is the boot cert), mints two tokens signed with Dex's active signing key,
# and runs the checks.
isolation_setup_tenants() {
  local work="$1"
  # `sycoadm setup` installed the bundled Dex on the pinned NodePort the apiserver
  # trusts, with its own TLS cert.
  wait_for "bundled Dex ready" 120 \
    "kubectl get deploy sycophant-dex -n $APISERVER_OIDC_NS -o jsonpath='{.status.readyReplicas}' 2>/dev/null | grep -qx 1"
  # Dex creates its signing key singleton on first serve; wait for it before
  # reading the key to sign tokens.
  wait_for "Dex signing key" 60 \
    "kubectl get signingkeies openid-connect-keys -n $APISERVER_OIDC_NS >/dev/null 2>&1"
}

isolation_mint_tokens() {
  local work="$1"
  # Two Dex-signed tokens: a positive one whose sub is tenant A's bound
  # identity, and a negative one whose sub carries the reserved system: prefix.
  local token
  token="$(mint_dex_token "$TENANT_A_USER_SUB")"
  systoken="$(mint_dex_token "system:masters")"

  user_identity="${APISERVER_OIDC_ISSUER}#${TENANT_A_USER_SUB}"

  ukube="$work/user.kubeconfig"
  kubeconfig_with_token "$ukube" user "$token"

  # Authn: the token must authenticate as the issuer-prefixed identity. The
  # apiserver's OIDC authenticator initializes lazily on first token use: it
  # needs the discovery/JWKS endpoint reachable, then a few seconds to finish,
  # and answers "authenticator not initialized" / 401 until it does. Poll
  # whoami until the identity appears, treating those transient answers as
  # retry, and fail only when the budget is spent.
  wait_for "user token OIDC authenticator initialized" 60 \
    "kubectl --kubeconfig='$ukube' auth whoami -o jsonpath='{.status.userInfo.username}' 2>/dev/null | grep -qFx '$user_identity'" \
    || { warn "user token did not authenticate as ${user_identity} within the init window (last: $(kubectl --kubeconfig="$ukube" auth whoami -o jsonpath='{.status.userInfo.username}' 2>/dev/null || echo 401/none))"; exit 1; }
  ok "user token authenticated as ${user_identity}"

  local groups
  groups="$(kubectl --kubeconfig="$ukube" auth whoami -o jsonpath='{.status.userInfo.groups}')"
  case "$groups" in
    *'"sycophant:tenant-admins"'*) ok "a login through the configured issuer carries the sycophant:tenant-admins group" ;;
    *) warn "a login through the configured issuer lacks the sycophant:tenant-admins group (groups: ${groups})"; exit 1 ;;
  esac
}

isolation_authz_checks() {
  local ukube="$1" systoken="$2"
  # A system:-prefixed sub is refused at authentication: the issuer prefix bars
  # it structurally and the claimValidationRules guard rejects the raw claim.
  KUBECONFIG="$ukube" kubectl config set-credentials user --token="$systoken" >/dev/null
  local sysout
  if sysout="$(kubectl --kubeconfig="$ukube" auth whoami -o jsonpath='{.status.userInfo.username}' 2>&1)"; then
    warn "a system: token authenticated as ${sysout}; the reserved-prefix guard did not fire"; exit 1
  fi
  ok "a system: token is refused at authentication"
}

isolation_verify_bindings() {
  local user_identity="$1"
  # No login-time mint: the only binding naming the user identity is the user
  # provision command's user-<hash> binding.
  local rb_naming
  rb_naming="$(kubectl get rolebindings -n "$NAMESPACE" -o json 2>/dev/null \
    | IDENT="$user_identity" python3 -c 'import json, os, sys
d = json.load(sys.stdin); ident = os.environ["IDENT"]
print("\n".join(rb["metadata"]["name"] for rb in d["items"]
      if any(s.get("kind") == "User" and s.get("name") == ident
             for s in rb.get("subjects") or [])))')"
  local user_hash; user_hash="$(handle_of "$user_identity")"
  local user_rb="user-${user_hash}"
  # Every binding naming the identity must come from the user provision
  # command's own release: the membership binding user-<hash>, or a reach
  # binding user-ws-<hash>-<workspace>. A binding outside that shape is a mint.
  local foreign_rb
  foreign_rb="$(printf '%s\n' "$rb_naming" | grep -vE "^(${user_rb}|user-ws-${user_hash}-[a-z0-9]([-a-z0-9]*[a-z0-9])?)$" || true)"
  [ -z "$foreign_rb" ] \
    || { warn "user identity ${user_identity} is named by RoleBinding(s) outside ${user_rb}(-ws-<workspace>): ${foreign_rb:-none}"; exit 1; }
  ok "no per-identity binding minted at login; every binding naming the user comes from the user provision command"
}

verify_user_isolation() {
  step "Per-identity cluster-identity isolation proof"
  command -v python3 >/dev/null 2>&1 || { warn "python3 required for the identity proof"; exit 1; }

  [ -s "$SYCO_DEX_CA" ] \
    || { warn "apiserver OIDC trust material missing at ${SYCO_DEX_CA}; boot the cluster with --clean first"; exit 1; }

  local work; work="$(mktemp -d)"
  trap 'rm -rf "$work"; trap - RETURN' RETURN

  isolation_setup_tenants "$work"

  local ukube systoken user_identity
  isolation_mint_tokens "$work"

  isolation_authz_checks "$ukube" "$systoken"
  isolation_verify_bindings "$user_identity"

  ok "per-identity cluster-identity isolation proof passed"
}

# ---- instructions delivery proof ----
readonly INSTRUCTIONS_PROBE_WS="instructions-probe"

# Sorted "sha256  relpath" lines for the regular files under directory $1.
tree_manifest() {
  python3 -c 'import hashlib, os, sys
root, out = sys.argv[1], []
for d, _, fs in os.walk(root):
    for f in fs:
        p = os.path.join(d, f)
        if os.path.isfile(p) and not os.path.islink(p):
            out.append(hashlib.sha256(open(p, "rb").read()).hexdigest() + "  " + os.path.relpath(p, root))
print("\n".join(sorted(out)))' "$1"
}

# Wait for workspace $2's harness Deployment (namespace $1) to finish rolling
# out, then print the name of its one live (not terminating) pod.
harness_pod() {
  local ns="$1" ws="$2" sel pods
  sel="app.kubernetes.io/component=harness,sycophant.md/workspace=${ws}"
  if ! kubectl rollout status "deploy/ws-${ws}" -n "$ns" --timeout=180s > /dev/null; then
    warn "deploy/ws-${ws} in ${ns} did not finish rolling out"
    return 1
  fi
  if ! wait_for "one live ws-${ws} harness pod in ${ns}" 120 \
    "[ \"\$(kubectl get pods -n '$ns' -l '$sel' -o json | jq '[.items[] | select(.metadata.deletionTimestamp == null)] | length')\" = 1 ]"; then
    return 1
  fi
  if ! pods="$(kubectl get pods -n "$ns" -l "$sel" -o json \
    | jq -r '.items[] | select(.metadata.deletionTimestamp == null) | .metadata.name')"; then
    warn "listing ws-${ws}'s harness pods in ${ns} failed"
    return 1
  fi
  printf '%s' "$pods"
}

# attach_reader <namespace> <pod> <container-name> <label>: attach an
# ephemeral busybox to the pod and wait until it answers exec. <label> names
# it in the warnings.
attach_reader() {
  local ns="$1" pod="$2" c="$3" label="$4"
  if ! attach_ephemeral_scrubber "$ns" "$pod" "$c"; then
    warn "attaching ${label} to ${ns}/${pod} failed"
    return 1
  fi
  wait_for "${label} attached to ${ns}/${pod}" 90 \
    "kubectl exec -n '$ns' '$pod' -c '$c' -- true >/dev/null 2>&1"
}

# The "sha256  relpath" manifest of the instructions directory inside
# workspace $2's live harness pod (namespace $1); empty for an empty
# directory. The harness image has no shell, so an ephemeral busybox sharing
# its PID namespace reads it through /proc/1/root, as the scrubber does in
# step 6. The init container syncs the store folder into a fresh emptyDir on
# every pod start, so this is the folder's content as the pod received it.
pod_instructions_manifest() {
  local ns="$1" ws="$2" pod c out
  if ! pod="$(harness_pod "$ns" "$ws")"; then
    return 1
  fi
  c="instr-read-$(date +%s)-${RANDOM}"
  if ! attach_reader "$ns" "$pod" "$c" "an instructions reader"; then
    return 1
  fi
  if ! out="$(kubectl exec -n "$ns" "$pod" -c "$c" -- \
    sh -c "cd /proc/1/root/etc/instructions/${ws} && find . -type f -exec sha256sum {} +")"; then
    warn "reading ${ns}/${pod}'s instructions directory failed"
    return 1
  fi
  if [ -z "$out" ]; then
    return 0
  fi
  printf '%s\n' "$out" | sed 's#  \./#  #' | LC_ALL=C sort
}

# Workspace $2's live pod (namespace $1) holds exactly the tree whose manifest
# is $3; $4 names the case.
assert_pod_holds() {
  local ns="$1" ws="$2" want="$3" label="$4" got
  if ! got="$(pod_instructions_manifest "$ns" "$ws")"; then
    warn "${label}: reading ${ns}/${ws}'s pod instructions directory failed"
    return 1
  fi
  if [ "$got" != "$want" ]; then
    warn "${label}: ${ns}/${ws}'s pod instructions directory holds [${got//$'\n'/; }], want [${want//$'\n'/; }]"
    return 1
  fi
}

# The pod uid and Deployment generation of workspace $2's harness (namespace
# $1), as "uid/generation".
harness_pod_identity() {
  local ns="$1" ws="$2" pod uid gen
  if ! pod="$(harness_pod "$ns" "$ws")"; then
    return 1
  fi
  if ! uid="$(kubectl get pod "$pod" -n "$ns" -o jsonpath='{.metadata.uid}')" || [ -z "$uid" ]; then
    warn "reading ${ns}/${pod}'s uid failed"
    return 1
  fi
  if ! gen="$(kubectl get deploy "ws-${ws}" -n "$ns" -o jsonpath='{.metadata.generation}')" || [ -z "$gen" ]; then
    warn "reading deploy/ws-${ws}'s generation in ${ns} failed"
    return 1
  fi
  printf '%s/%s' "$uid" "$gen"
}

# Hello-world's pod holds exactly the example instructions tree.
instructions_baseline() {
  local example="$REPO_ROOT/examples/sycoadm/instructions/hello-world" want
  if [ ! -f "$example/agents/poet.md" ]; then
    warn "the example tree has no file in a subdirectory (agents/poet.md)"
    return 1
  fi
  if ! want="$(tree_manifest "$example")" || [ -z "$want" ]; then
    warn "reading the example instructions tree failed"
    return 1
  fi
  if ! assert_pod_holds "$NAMESPACE" hello-world "$want" "baseline"; then
    return 1
  fi
  ok "hello-world's pod holds exactly the example instructions, delivered by sycoadm instructions up"
}

instructions_probe() {
  local ns="$NAMESPACE" ws="$INSTRUCTIONS_PROBE_WS" tenant="${NAMESPACE#tenant-}"
  local d; d="$(mktemp -d)"
  trap 'rm -rf "'"$d"'"; trap - RETURN' RETURN
  local want before after

  local tree="$d/tree"
  cp -R "$REPO_ROOT/examples/sycoadm/instructions/hello-world" "$tree"
  env WORKSPACE_NAME="$ws" WORKSPACE_TENANT="$tenant" \
    envsubst '${WORKSPACE_NAME} ${WORKSPACE_TENANT}' \
    < "$REPO_ROOT/tests/fixtures/instructions/workspace.yaml" > "$d/workspace.yaml"
  env WORKSPACE_NAME="$ws" WORKSPACE_TENANT="$tenant" INSTRUCTIONS_DIR="$tree" \
    envsubst '${WORKSPACE_NAME} ${WORKSPACE_TENANT} ${INSTRUCTIONS_DIR}' \
    < "$REPO_ROOT/tests/fixtures/instructions/instructions.yaml" > "$d/instructions.yaml"
  if ! "$SYCO" workspace up -f "$d/workspace.yaml"; then
    warn "workspace up of ${ws} failed"
    return 1
  fi
  if ! "$SYCO" instructions up -f "$d/instructions.yaml"; then
    warn "instructions up of ${ws} with ${tree} failed"
    return 1
  fi
  if ! want="$(tree_manifest "$tree")" || [ -z "$want" ]; then
    warn "reading the probe instructions tree failed"
    return 1
  fi
  if ! assert_pod_holds "$ns" "$ws" "$want" "probe precondition"; then
    return 1
  fi

  if ! before="$(harness_pod_identity "$ns" "$ws")"; then
    return 1
  fi
  printf 'You are the changed probe.\n' > "$tree/AGENTS.md"
  rm "$tree/agents/poet.md"
  if ! "$SYCO" instructions up -f "$d/instructions.yaml"; then
    warn "instructions up of ${ws} with changed instructions failed"
    return 1
  fi
  if ! after="$(harness_pod_identity "$ns" "$ws")"; then
    return 1
  fi
  if [ "${before%/*}" = "${after%/*}" ]; then
    warn "changed instructions did not replace the probe pod (uid ${before%/*})"
    return 1
  fi
  if ! want="$(tree_manifest "$tree")" || [ -z "$want" ]; then
    warn "reading the changed probe instructions tree failed"
    return 1
  fi
  if ! assert_pod_holds "$ns" "$ws" "$want" "changed files"; then
    return 1
  fi
  ok "changed and deleted instruction files restarted the pod; the new pod holds exactly the new set"

  if ! "$SYCO" instructions down "$ws" --tenant "$tenant"; then
    warn "instructions down ${ws} --tenant ${tenant} failed"
    return 1
  fi
  if ! assert_pod_holds "$ns" "$ws" "" "instructions down"; then
    return 1
  fi
  ok "instructions down left the probe pod holding no instructions"

  if ! "$SYCO" workspace down "$ws" --tenant "$tenant"; then
    warn "workspace down ${ws} --tenant ${tenant} failed"
    return 1
  fi
}

verify_instructions_delivery() {
  step "Instructions delivery proof (sycoadm instructions up/down)"
  instructions_baseline
  instructions_probe
  ok "instructions delivery proof passed"
}

# ---- tenant lifecycle command-contract proof ----
# Provision drives the tenant Namespace, quota, and base install; workspace and
# toolset provision commands each drive one release; suspend scales the tenant
# down and stamps a reap TTL the reap policy acts on.

# The 10-hex handle of $1: a user provision command's name suffix, and the
# chart's per-user RoleBinding suffix.
handle_of() { printf '%s' "$1" | openssl dgst -sha256 | awk '{print $NF}' | cut -c1-10; }

# The lifecycle workspace's harness pod name and Deployment generation, which a
# live toolset add must leave unchanged.
harness_identity() {
  printf '%s/%s' \
    "$(kubectl get pods -n "$LIFECYCLE_TENANT_NS" \
      -l "app.kubernetes.io/component=harness,sycophant.md/workspace=${LIFECYCLE_WORKSPACE}" \
      -o jsonpath='{.items[0].metadata.name}' 2>/dev/null)" \
    "$(kubectl get deploy "ws-$LIFECYCLE_WORKSPACE" -n "$LIFECYCLE_TENANT_NS" \
      -o jsonpath='{.metadata.generation}' 2>/dev/null)"
}

# The digest image the lifecycle toolset provision command for toolset $1
# carries. No tool is called, so nothing pulls it.
lifecycle_toolset_image() {
  printf 'registry.local/%s@sha256:%064d' "$1" 0
}

# Write the values for toolset $1 to $work.
toolset_values() {
  TOOLSET_NAME="$1" \
  TOOLSET_TENANT="$LIFECYCLE_TENANT_TOKEN" \
  TOOLSET_IMAGE="$(lifecycle_toolset_image "$1")" \
  envsubst < "$REPO_ROOT/tests/fixtures/lifecycle/toolset.yaml" > "$work/toolset-$1.yaml"
}

lifecycle_tenant_up() {
  local work="$1"
  # Start from nothing, so the proof covers a first provision. A prior run's
  # installer outlives its reap TTL.
  kubectl delete configmap "provision-tenant-${LIFECYCLE_TENANT_TOKEN}" -n sycophant-system --ignore-not-found >/dev/null
  kubectl delete namespace "$LIFECYCLE_TENANT_NS" --ignore-not-found --timeout=300s >/dev/null

  local operator_identity="${APISERVER_OIDC_ISSUER}#${LIFECYCLE_OPERATOR_SUB}"
  local owner_hash
  owner_hash="$(printf '%s' "$operator_identity" | openssl dgst -sha256 | awk '{print $NF}' | cut -c1-16)"
  local tenant_values=(-f "$REPO_ROOT/examples/sycoadm/tenants/hello-world.yaml"
    --set "tenant.name=$LIFECYCLE_TENANT_TOKEN" --set tenant.capacity=512Mi
    --set "tenant.owner=$operator_identity" --set "tenant.ownerHash=$owner_hash")

  # The operator's real path: sycoadm writes the tenant provision command under its
  # Dex login and waits on its installer Job, failing loudly on a refused
  # command or a failed Job.
  syco_credentials
  "$SYCO" tenant up "${tenant_values[@]}"
  ok "sycoadm tenant up: tenant provision command admitted for the operator's Dex identity; its installer Job succeeded"

  wait_for "tenant namespace generated" 120 \
    "kubectl get namespace '$LIFECYCLE_TENANT_NS' >/dev/null 2>&1"
  wait_for "tenant capacity quota generated" 120 \
    "kubectl get resourcequota '${LIFECYCLE_TENANT_NS}-capacity' -n '$LIFECYCLE_TENANT_NS' >/dev/null 2>&1"
  kubectl get namespace "$LIFECYCLE_TENANT_NS" \
    -o jsonpath='{.metadata.labels.app\.kubernetes\.io/part-of}' 2>/dev/null | grep -qx sycophant-tenant \
    || { warn "generated namespace lacks the part-of=sycophant-tenant label"; exit 1; }
  ok "provision generated the tenant namespace and capacity quota"

  wait_for "relay-controller Deployment Available" 180 \
    "kubectl get deploy relay-controller -n '$LIFECYCLE_TENANT_NS' -o jsonpath='{.status.availableReplicas}' 2>/dev/null | grep -qx 1"
  ok "relay-controller is Available"

  WORKSPACE_NAME="$LIFECYCLE_WORKSPACE" \
  WORKSPACE_TENANT="$LIFECYCLE_TENANT_TOKEN" \
  envsubst < "$REPO_ROOT/tests/fixtures/lifecycle/workspace.yaml" > "$work/workspace.yaml"
  "$SYCO" workspace up -f "$work/workspace.yaml"

  # The harness's one init container, instructions-sync, rclone-syncs the
  # instructions prefix; a clean exit proves the store sync end to end.
  wait_for "harness instructions-sync init container completed" 180 \
    "kubectl get pods -n '$LIFECYCLE_TENANT_NS' -l 'app.kubernetes.io/component=harness,sycophant.md/workspace=${LIFECYCLE_WORKSPACE}' -o jsonpath='{.items[0].status.initContainerStatuses[0].state.terminated.exitCode}' 2>/dev/null | grep -qx 0"
  kubectl get pods -n "$LIFECYCLE_TENANT_NS" \
    -l "app.kubernetes.io/component=harness,sycophant.md/workspace=${LIFECYCLE_WORKSPACE}" \
    -o jsonpath='{.items[0].status.initContainerStatuses[0].name}' 2>/dev/null | grep -qx instructions-sync \
    || { warn "the harness first init container is not instructions-sync; the exit-0 check inspected the wrong container"; exit 1; }
  ok "harness instructions-sync init container (rclone) synced and exited 0"
}

lifecycle_verify_running() {
  local work="$1"
  # The base release's install-wait hook holds its installer Job until the
  # relay-controller is Available, so it is serving before the workspace
  # provision command and merges the new grants live.
  wait_for "relay-controller served the new workspace grants live" 60 \
    "kubectl logs -n '$LIFECYCLE_TENANT_NS' -l app.kubernetes.io/name=relay-controller --tail=80 2>/dev/null | grep -E 'toolset-grants delivery applied.*\"configmaps\":[1-9]' >/dev/null"
  ok "relay-controller merged the workspace grants without a pod roll"

  # One toolset, one provision command. The harness merges the per-toolset
  # ConfigMap by watch, so its pod and Deployment generation must not change.
  local harness_before ts="$LIFECYCLE_TOOLSET"
  harness_before="$(harness_identity)"
  [ -n "${harness_before%/*}" ] || { warn "no lifecycle harness pod before the toolset add"; exit 1; }
  toolset_values "$ts"
  "$SYCO" toolset up -f "$work/toolset-${ts}.yaml"

  wait_for "harness merged toolset ${ts} by watch" 120 \
    "kubectl logs deploy/ws-${LIFECYCLE_WORKSPACE} -n ${LIFECYCLE_TENANT_NS} -c harness --tail=200 2>/dev/null | grep -E 'toolset delivery applied.*\"configmaps\":1,' >/dev/null"
  local harness_after; harness_after="$(harness_identity)"
  [ "$harness_before" = "$harness_after" ] \
    || { warn "harness pod or Deployment generation changed on toolset add (${harness_before} -> ${harness_after})"; exit 1; }
  ok "harness picked up toolset ${ts} live with no pod roll"
}

lifecycle_tenant_down() {
  # --- suspend: scale tenant Deployments to zero, stamp the reap TTL.
  kubectl apply -n "$LIFECYCLE_TENANT_NS" -f "$REPO_ROOT/tests/fixtures/lifecycle/workload.yaml" >/dev/null

  # `sycoadm tenant down` suspends with no keepalive; `sycoadm tenant remove`
  # re-writes the suspend command with keepalive 0.
  "$SYCO" tenant down "$LIFECYCLE_TENANT_TOKEN"
  wait_for "tenant deployments scaled to zero" 120 \
    "kubectl get deploy lifecycle-workload -n '$LIFECYCLE_TENANT_NS' -o jsonpath='{.spec.replicas}' 2>/dev/null | grep -qx 0" \
    || exit 1
  ok "sycoadm tenant down scaled the tenant deployments to zero"
}

lifecycle_tenant_remove() {
  "$SYCO" tenant remove "$LIFECYCLE_TENANT_TOKEN"
  wait_for "reap-after TTL stamped on the namespace" 120 \
    "kubectl get namespace '$LIFECYCLE_TENANT_NS' -o jsonpath='{.metadata.annotations.sycophant\.md/reap-after}' 2>/dev/null | grep -q ." \
    || exit 1
  ok "sycoadm tenant remove stamped the reap TTL"
}

lifecycle_verify_cleanup() {
  # The e2e reap schedule is every minute (Kyverno's floor); allow a loaded node.
  wait_for "tenant namespace reaped" 720 \
    "! kubectl get namespace '$LIFECYCLE_TENANT_NS' >/dev/null 2>&1"
  ok "the zero-keepalive tenant namespace was reaped"

  ok "tenant lifecycle command-contract proof passed"
}

verify_tenant_lifecycle() {
  step "Tenant lifecycle command-contract proof"
  command -v python3 >/dev/null 2>&1 || { warn "python3 required for the lifecycle proof"; exit 1; }

  local work; work="$(mktemp -d)"
  trap 'rm -rf "$work"; trap - RETURN' RETURN

  lifecycle_tenant_up "$work"
  lifecycle_verify_running "$work"
  lifecycle_tenant_down
  lifecycle_tenant_remove
  lifecycle_verify_cleanup
}

main() {
  # The operator path below runs through this checkout's sycoadm.
  cargo build --release -p sycoadm >/dev/null
  # The user CLI (Go) must also be current for steps that call syco.
  (cd "$REPO_ROOT/cmd/syco" && go build -o "$REPO_ROOT/target/syco" .) \
    || { warn "go build of cmd/syco failed"; return 1; }
  local cluster_exists=0
  if k3d cluster list 2>/dev/null | awk 'NR>1{print $1}' | grep -qx "$CLUSTER_NAME"; then
    cluster_exists=1
  fi

  if [ "$CLEAN" = "1" ]; then
    step "Clean run — fresh cluster + full image rebuild"
    env CLEAN=1 "$REPO_ROOT/scripts/preflight.sh"
    step_0_bootstrap
    step_1_build
  elif [ "$cluster_exists" = "1" ]; then
    warn "Reusing cluster '$CLUSTER_NAME' + its loaded images (pass --clean to rebuild from scratch)"
    # Reuse still runs deploy, the proofs, and upgrade, so the runtime tools are
    # required; only the cross-build tier is not. Check them (CLEAN=0 skips musl).
    env CLEAN=0 "$REPO_ROOT/scripts/preflight.sh"
    kubectl config use-context "k3d-$CLUSTER_NAME" >/dev/null 2>&1 \
      || { warn "could not switch kube-context to k3d-$CLUSTER_NAME — the k3d cluster exists but its kubeconfig context is missing; recreate with --clean"; exit 1; }
    step "Reuse run — sycoadm setup re-applies this checkout's cluster chart before the tenant deploy"
    sycoadm_setup
  else
    warn "No cluster '$CLUSTER_NAME' — bootstrapping once (later runs without --clean reuse it)"
    # First boot bootstraps and cross-compiles the images, so it needs the full
    # build tier even though the run was not invoked with --clean.
    env CLEAN=1 "$REPO_ROOT/scripts/preflight.sh"
    step_0_bootstrap
    step_1_build
  fi

  syco_credentials
  step_3_deploy
  verify_user_isolation
  verify_instructions_delivery
  verify_tenant_lifecycle
  step_4_verify
  step_5_channel
  step_6_security
  step_7_upgrade_cli
  if [ "$_step_start" -gt 0 ]; then
    printf '\033[1;34m    ⏱  previous step took %ds\033[0m\n' "$(( SECONDS - _step_start ))"
  fi
  printf '\n\033[1;32m==> e2e complete (%ds total)\033[0m\n' "$SECONDS"
  # Each helper installs a function-local `trap ... RETURN` to clean its temp
  # dir. Without functrace that trap stays installed after the helper returns
  # and would fire again on main's own return, when its `work` is out of scope
  # and set -u aborts. Clear it so main returns cleanly.
  trap - RETURN
}

main "$@"

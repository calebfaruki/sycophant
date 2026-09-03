#!/usr/bin/env bash
# Single-command end-to-end test for sycophant.
#
# Bootstraps a clean k3d cluster, builds + loads all images, deploys the
# Helm charts (in-cluster headscale plus the app-channel adapter), launches
# the Flutter client, and runs the Step 6 security assertions. Pauses for
# the operator at the Flutter UI step.
#
# The app channel's terminus is its own adapter Deployment, so the run
# stands up headscale, mints a pre-auth key into the adapter's authKey
# Secret, and asserts the adapter reaches Available under gVisor.
#
# Required env:
#   OPENROUTER_API_KEY
# Optional env:
#   CLUSTER_NAME (default sycophant-dev), NAMESPACE (default e2e-test),
#   ARCH (default aarch64), DOCKER_ARCH (default arm64).
set -euo pipefail

REPO_ROOT="$(git rev-parse --show-toplevel)"
CLUSTER_NAME="${CLUSTER_NAME:-sycophant-dev}"
NAMESPACE="${NAMESPACE:-e2e-test}"
ARCH="${ARCH:-aarch64}"
DOCKER_ARCH="${DOCKER_ARCH:-arm64}"
RUST_TARGET="${ARCH}-unknown-linux-musl"
# The operator invents the grant row's identity and hands it over out of
# band. The relay mints nothing, so this string is the whole proof.
# The producer is bounded, not the consumer: `tr </dev/urandom | head -c 16`
# hands `tr` a SIGPIPE the moment `head` has its 16 bytes, and under
# `pipefail` that aborts the script at line 1.
GRANT_CODE="e2e-$(head -c 256 /dev/urandom | LC_ALL=C tr -dc 'a-zA-Z0-9' | cut -c1-16)"
HEADSCALE_USER="e2e"
# The client reaches the relay through the app adapter's tailnet node, at the
# adapter's MagicDNS hostname. Nothing dials the relay's app port directly.
TAILNET_RELAY_ADDR="relay:9090"
ADAPTER_AUTHKEY_SECRET="relay-tsnet-authkey"
K3D_NODE="k3d-${CLUSTER_NAME}-server-0"
# The in-cluster inference profile key. Its prompt profile points baseUrl at the
# inference-<key> Service, and the chart renders the llama.cpp Deployment, its
# ingress fence (CNP inference-<key>), and the prompt job's in-cluster egress
# hole (CNP toolset-<key>) from it. Its listen port is the one in that baseUrl.
INFERENCE_PROFILE="local"
INFERENCE_PORT="8080"
# Client choice. Prompt when unset + interactive (the "confirm during install"
# step); honor the env var otherwise so CI/agents run non-interactively.
# none = backend-only: skip the local Flutter client and connect one remotely.
if [ -z "${FLUTTER_TARGET:-}" ]; then
  if [ -t 0 ]; then
    printf 'Install a Flutter client? [macos/none] (none = backend-only): ' >&2
    read -r FLUTTER_TARGET
    FLUTTER_TARGET="${FLUTTER_TARGET:-none}"
  else
    FLUTTER_TARGET="macos"
  fi
fi
case "$FLUTTER_TARGET" in
  macos)   CLIENT_NAME_DEFAULT="caleb-macbook" ;;
  none)    CLIENT_NAME_DEFAULT="remote-client" ;;
  *) printf 'unknown FLUTTER_TARGET: %s (expected macos|none)\n' "$FLUTTER_TARGET" >&2; exit 1 ;;
esac
CLIENT_NAME="${CLIENT_NAME:-$CLIENT_NAME_DEFAULT}"

: "${OPENROUTER_API_KEY:?must be set}"

# ---- ui helpers ----
step()  { printf '\n\033[1;36m==> %s\033[0m\n' "$*"; }
ok()    { printf '\033[1;32m ✓\033[0m %s\n' "$*"; }
warn()  { printf '\033[1;33m ⚠\033[0m %s\n' "$*" >&2; }
pause() {
  local marker="/tmp/sycophant-e2e-continue"
  rm -f "$marker"
  printf '\n\033[1;33m🛑 %s\033[0m\n' "$1"
  printf '   When done, run: \033[1mtouch %s\033[0m\n' "$marker"
  until [ -f "$marker" ]; do sleep 2; done
  rm -f "$marker"
}

# wait_for <description> <timeout-seconds> <shell-condition>
# Polls the condition every 2 s until it succeeds OR the timeout expires.
# Returns non-zero on timeout, with a `warn` log. Use this instead of an
# unbounded `until ... ; do sleep N; done` so hangs surface as failures.
wait_for() {
  local desc="$1"; local timeout="$2"; shift 2
  local deadline=$(( $(date +%s) + timeout ))
  while ! eval "$*"; do
    if [ "$(date +%s)" -ge "$deadline" ]; then
      warn "timeout after ${timeout}s waiting for: ${desc}"
      return 1
    fi
    sleep 2
  done
}

# ---- background cleanup (port-forwards, flutter run) ----
declare -a CLEANUP_PIDS=()
# Set once the headscale forward is up. The cluster outlives the script but
# the forward does not, so a client still driving the app loses the tailnet
# the moment we exit — say how to get it back.
HEADSCALE_FORWARD_STARTED=""
cleanup() {
  local pid
  for pid in "${CLEANUP_PIDS[@]:-}"; do
    [ -n "$pid" ] && kill "$pid" 2>/dev/null || true
  done
  if [ -n "$HEADSCALE_FORWARD_STARTED" ]; then
    printf '\nThe headscale port-forward stopped with this script. To keep using\n' >&2
    printf 'the app against the running cluster, restore it with:\n' >&2
    printf '  kubectl port-forward -n %s svc/headscale 8080:8080\n' "$NAMESPACE" >&2
  fi
}
trap cleanup EXIT

# Defensive: kill leftover sycophant processes from prior runs (leaked
# cargo test binaries, abandoned kubectl port-forwards, respawn loops).
"$REPO_ROOT/scripts/kill-orphans.sh"

# ---- step 0: bootstrap ----
step_0_bootstrap() {
  step "Step 0: Bootstrap cluster"

  k3d cluster delete "$CLUSTER_NAME" 2>/dev/null || true

  k3d cluster create "$CLUSTER_NAME" \
    --k3s-arg "--flannel-backend=none@server:*" \
    --k3s-arg "--disable-network-policy@server:*" \
    --k3s-arg "--disable=traefik@server:*" \
    --k3s-arg "--disable=servicelb@server:*" \
    --k3s-arg "--disable=metrics-server@server:*" \
    --k3s-arg "--disable=helm-controller@server:*" \
    --k3s-arg "--secrets-encryption@server:*" \
    --k3s-arg "--kube-apiserver-arg=audit-policy-file=/etc/rancher/k3s/audit-policy.yaml@server:*" \
    --k3s-arg "--kube-apiserver-arg=audit-log-path=/var/log/k3s-audit.log@server:*" \
    --k3s-arg "--kube-apiserver-arg=audit-log-maxage=7@server:*" \
    -v "$HOME/sycophant/tmp:$HOME/sycophant/tmp@all" \
    -v "$HOME/sycophant/docs/e2e/audit-policy.yaml:/etc/rancher/k3s/audit-policy.yaml@server:0" \
    --registry-create "sycophant-registry:0.0.0.0:5555" \
    --port "9090:9090@loadbalancer"
  ok "k3d cluster created"

  # gVisor must come before Cilium — HUP'ing k3s after Cilium is installed
  # crashes the cilium-agent (CRI socket disappears mid-restart).
  install_gvisor
  install_cilium
  # Must run after Cilium so CoreDNS pods can actually get IPs (otherwise
  # the rollout-restart hangs in Pending and times out).
  patch_coredns_for_registry
  smoke_gvisor
  install_kyverno
}

# Toolset-ctrl resolves toolset image refs from inside the cluster to read
# tool labels off the image manifest. CoreDNS doesn't know about the
# `sycophant-registry` container (it's on the k3d Docker network, not in
# Kubernetes Services), so we patch its NodeHosts to add the entry. Image
# refs in the toolsets values use `sycophant-registry:5000/...` (in-cluster name +
# port); without this patch, the hostname is NXDOMAIN and tool discovery
# fails silently — Step 6 then can't find any toolset tool execution.
patch_coredns_for_registry() {
  step "Step 0.2: CoreDNS NodeHosts for sycophant-registry"
  local registry_ip
  registry_ip="$(docker inspect sycophant-registry --format '{{ (index .NetworkSettings.Networks "k3d-'"$CLUSTER_NAME"'").IPAddress }}')"
  if [ -z "$registry_ip" ]; then
    warn "could not resolve sycophant-registry IP on k3d-${CLUSTER_NAME} network"
    return 1
  fi
  local current_hosts
  current_hosts="$(kubectl get cm coredns -n kube-system -o jsonpath='{.data.NodeHosts}')"
  if echo "$current_hosts" | grep -q "sycophant-registry"; then
    ok "CoreDNS NodeHosts already has sycophant-registry"
    return 0
  fi
  kubectl patch cm coredns -n kube-system --type=merge \
    --patch="{\"data\":{\"NodeHosts\":\"${current_hosts}\n${registry_ip} sycophant-registry\"}}" \
    >/dev/null
  kubectl rollout restart deploy/coredns -n kube-system >/dev/null
  kubectl rollout status deploy/coredns -n kube-system --timeout=180s >/dev/null
  local dns_deadline=$((SECONDS + 60))
  while (( SECONDS < dns_deadline )); do
    if kubectl run "dns-probe-$$" --rm -i --restart=Never --image=busybox:1.36 \
         --quiet --timeout=10s -- nslookup sycophant-registry >/dev/null 2>&1; then
      ok "CoreDNS resolves sycophant-registry -> ${registry_ip}"
      return 0
    fi
    sleep 2
  done
  warn "sycophant-registry did not resolve from a workload pod within 60s (continuing; toolset-ctrl will retry)"
}

install_gvisor() {
  step "Step 0.3: gVisor (runsc) install"
  local url="https://storage.googleapis.com/gvisor/releases/release/latest/${ARCH}"
  local tmp
  tmp="$(mktemp -d)"
  # --retry-all-errors covers a mid-transfer TCP reset (curl error 56), which
  # plain --retry does not; storage.googleapis.com resets intermittently.
  local retry='--retry 5 --retry-delay 2 --retry-all-errors --connect-timeout 10'
  ( cd "$tmp"
    curl -sSfL $retry -o runsc                            "$url/runsc"
    curl -sSfL $retry -o runsc.sha512                     "$url/runsc.sha512"
    curl -sSfL $retry -o containerd-shim-runsc-v1         "$url/containerd-shim-runsc-v1"
    curl -sSfL $retry -o containerd-shim-runsc-v1.sha512  "$url/containerd-shim-runsc-v1.sha512"
    sha512sum -c runsc.sha512 -c containerd-shim-runsc-v1.sha512
    chmod +x runsc containerd-shim-runsc-v1
    docker exec "$K3D_NODE" mkdir -p /usr/local/bin
    docker cp runsc                    "$K3D_NODE":/usr/local/bin/runsc
    docker cp containerd-shim-runsc-v1 "$K3D_NODE":/usr/local/bin/containerd-shim-runsc-v1
  )
  rm -rf "$tmp"

  docker exec "$K3D_NODE" sh -c 'cat > /var/lib/rancher/k3s/agent/etc/containerd/config-v3.toml.tmpl <<TMPL
{{ template "base" . }}

[plugins."io.containerd.cri.v1.runtime".containerd.runtimes.runsc]
  runtime_type = "io.containerd.runsc.v1"
TMPL'

  docker exec "$K3D_NODE" sh -c 'kill -HUP $(pidof k3s)'
  until kubectl get --raw /healthz 2>/dev/null | grep -q '^ok$'; do sleep 2; done

  helm upgrade --install sycophant-gvisor "$REPO_ROOT/charts/sycophant-gvisor" --wait >/dev/null
  ok "gVisor + RuntimeClass installed"
}

install_cilium() {
  step "Step 0.4: Cilium"
  local api_host
  # Index the specific k3d network -- ranging over all networks concatenates
  # IPs (garbage) when the node is attached to more than one Docker network.
  api_host="$(docker inspect "$K3D_NODE" -f '{{ (index .NetworkSettings.Networks "k3d-'"$CLUSTER_NAME"'").IPAddress }}')"
  helm repo add cilium https://helm.cilium.io/ >/dev/null
  helm repo update >/dev/null
  # k3d wipes the node image cache on every cluster delete, so the ~240MB Cilium
  # agent image is re-pulled from quay.io each run; a slow pull (minutes) outlasts
  # the rollout wait below. Import it from the host Docker cache first (the pull is
  # a fast no-op once cached, slow only on a cold host) so the install finds it
  # present by digest and never pulls at run time. The small operator-generic
  # image is left to pull normally: its multi-arch index references a blob absent
  # from a single-arch host pull, which breaks the tar import, and its size makes
  # a run-time pull cheap.
  docker pull -q quay.io/cilium/cilium:v1.19.3 >/dev/null
  local cilium_tar; cilium_tar="$(mktemp -t cilium.XXXXXX).tar"
  docker image save -o "$cilium_tar" quay.io/cilium/cilium:v1.19.3
  k3d image import "$cilium_tar" --cluster "$CLUSTER_NAME" >/dev/null
  rm -f "$cilium_tar"
  # Same Cilium config as `syco install`; only the API endpoint is dynamic.
  helm upgrade --install cilium cilium/cilium --version 1.19.3 \
    --namespace kube-system \
    -f "$REPO_ROOT/cli/values/cilium.yaml" \
    --set k8sServiceHost="$api_host" \
    --set k8sServicePort=6443 >/dev/null
  kubectl rollout status -n kube-system ds/cilium --timeout=300s >/dev/null
  kubectl rollout status -n kube-system deployment/cilium-operator --timeout=300s >/dev/null
  ok "Cilium ready"
}

smoke_gvisor() {
  step "Step 0.5: gVisor smoke test"
  kubectl delete pod gvisor-smoke --ignore-not-found --grace-period=0 --force >/dev/null 2>&1 || true
  kubectl run gvisor-smoke --restart=Never \
    --overrides='{"spec":{"runtimeClassName":"gvisor"}}' \
    --image=busybox:stable --command -- dmesg >/dev/null
  kubectl wait pod/gvisor-smoke --for=jsonpath='{.status.phase}'=Succeeded --timeout=60s >/dev/null
  if wait_for "'Starting gVisor' in gvisor-smoke logs" 10 \
       "kubectl logs pod/gvisor-smoke 2>/dev/null | grep -q 'Starting gVisor'"; then
    ok "gVisor sandbox boots"
  else
    warn "gVisor first dmesg line did NOT match 'Starting gVisor'"
    kubectl logs pod/gvisor-smoke | head -3
    return 1
  fi
  kubectl delete pod gvisor-smoke --ignore-not-found --grace-period=0 --force >/dev/null 2>&1 || true
}

install_kyverno() {
  step "Step 0.7: Kyverno"
  helm repo add kyverno https://kyverno.github.io/kyverno/ >/dev/null
  helm repo update >/dev/null
  helm upgrade --install kyverno kyverno/kyverno --version 3.5.3 -n kyverno --create-namespace --wait >/dev/null
  ok "Kyverno ready"
}

# ---- step 1: build images ----
step_1_build() {
  step "Step 1: Build images"
  cd "$REPO_ROOT"

  cargo build --release --target "$RUST_TARGET" \
    -p toolset-controller -p prompt-toolset \
    -p toolset-runtime \
    -p harness -p relay-controller

  local bin
  for bin in toolset-controller toolset-runtime relay-controller; do
    cp "target/$RUST_TARGET/release/$bin" "${bin}-linux-musl-${DOCKER_ARCH}"
    docker build -q -f build/Dockerfile \
      --build-arg "BINARY=$bin" --build-arg "TARGETARCH=$DOCKER_ARCH" \
      -t "${bin}:local" . >/dev/null
    rm "${bin}-linux-musl-${DOCKER_ARCH}"
  done

  # The one toolset base image: its entrypoint is the toolset runtime. Every
  # toolset image below, and the prompt image, builds FROM it.
  cp "target/$RUST_TARGET/release/toolset-runtime" "images/toolset-base/toolset-runtime-linux-${DOCKER_ARCH}"
  docker build -q --build-arg "TARGETARCH=$DOCKER_ARCH" -f images/toolset-base/Dockerfile images/toolset-base/ -t toolset-base:local >/dev/null
  rm "images/toolset-base/toolset-runtime-linux-${DOCKER_ARCH}"

  # The prompt toolset ships as a published image, not a locally mounted binary.
  cp "target/$RUST_TARGET/release/prompt-toolset" "images/prompt/prompt-toolset-linux-${DOCKER_ARCH}"
  docker build -q --build-arg "TARGETARCH=$DOCKER_ARCH" --build-arg "BASE_IMAGE=toolset-base:local" \
    -f images/prompt/Dockerfile images/prompt/ -t prompt-toolset:local >/dev/null
  rm "images/prompt/prompt-toolset-linux-${DOCKER_ARCH}"

  cp "target/$RUST_TARGET/release/harness" "harness-linux-musl-${DOCKER_ARCH}"
  docker build -q -f build/Dockerfile \
    --build-arg BINARY=harness --build-arg "TARGETARCH=$DOCKER_ARCH" \
    -t sycophant-harness:local . >/dev/null
  rm "harness-linux-musl-${DOCKER_ARCH}"

  docker build -q --build-arg "BASE_IMAGE=toolset-base:local" -f images/toolset/Dockerfile images/toolset/ -t toolset:local >/dev/null

  docker build -q --build-arg "BASE_IMAGE=toolset-base:local" -f images/git/Dockerfile images/git/ -t toolset-git:local >/dev/null

  docker build -q --build-arg "BASE_IMAGE=toolset-base:local" -f examples/toolsets/ssh-credentials/Dockerfile examples/toolsets/ssh-credentials/ -t toolset-ssh-credentials:local >/dev/null

  docker build -q --build-arg "TARGETARCH=$DOCKER_ARCH" images/kubectl/ -t sycophant-kubectl:local >/dev/null

  # llama-server is a third-party engine: pulled by digest from trusted upstream,
  # never built here. --platform pins one arch so k3d import gets a single-arch
  # manifest, not a multi-arch index with absent per-platform blobs.
  # Operator places this GGUF here; source: https://huggingface.co/bartowski/Qwen_Qwen3-1.7B-GGUF (Qwen_Qwen3-1.7B-Q4_K_M.gguf)
  local GGUF_PATH="${GGUF_PATH:-${HOME}/.cache/sycophant/weights/Qwen3-1.7B-Q4_K_M.gguf}"
  local LLAMA_SERVER_REF="${LLAMA_SERVER_REF:-ghcr.io/ggml-org/llama.cpp:server@sha256:9f84380be42d6285a827629c809387349c3541aa8986f7536547ca33cc8dd47a}"
  docker pull -q --platform "linux/${DOCKER_ARCH}" "$LLAMA_SERVER_REF" >/dev/null
  docker tag "$LLAMA_SERVER_REF" llama-server:local
  docker build -q -f build/weights.Dockerfile \
    --build-arg "GGUF=$(basename "$GGUF_PATH")" \
    --build-arg "WEIGHTS_PATH=/weights/model.gguf" \
    -t weights:local "$(dirname "$GGUF_PATH")" >/dev/null

  step "Loading images into k3d + pushing toolsets to registry"
  local img
  for img in toolset-controller:local prompt-toolset:local \
             sycophant-harness:local relay-controller:local \
             sycophant-kubectl:local \
             weights:local; do
    k3d image import "$img" --cluster "$CLUSTER_NAME" >/dev/null
  done
  # llama-server is a multi-arch index under the containerd store; a plain import
  # saves manifests for absent platforms and fails. Export one arch to a tarball.
  local llama_tar; llama_tar="$(mktemp -t llama-server.XXXXXX).tar"
  docker image save --platform "linux/${DOCKER_ARCH}" -o "$llama_tar" llama-server:local
  k3d image import "$llama_tar" --cluster "$CLUSTER_NAME" >/dev/null
  rm -f "$llama_tar"
  # Toolset images go through the local registry (sycophant-registry:5000
  # in-cluster) so toolset-controller can fetch their OCI manifests for
  # tool discovery. The stdlib toolset rides the same path.
  for img in toolset toolset-git toolset-ssh-credentials; do
    docker tag "${img}:local" "localhost:5555/${img}:latest"
    docker push -q "localhost:5555/${img}:latest" >/dev/null
  done
  ok "Images built + loaded"
}

# ---- step 2: configure ----
step_2_configure() {
  step "Step 2: Configure namespace, RBAC, kernels, secrets"

  # Stamp the PSA labels at creation so the ns is restricted from birth. The
  # part-of label (added in step_3, after Kyverno is up) is what the VAP gates
  # on; a bare ns labelled part-of later would be denied for lacking enforce.
  kubectl create namespace "$NAMESPACE" --dry-run=client -o yaml \
    | kubectl label --local -f - -o yaml \
        pod-security.kubernetes.io/enforce=restricted \
        pod-security.kubernetes.io/enforce-version=latest \
        pod-security.kubernetes.io/warn=restricted \
        pod-security.kubernetes.io/audit=restricted \
    | kubectl apply -f - >/dev/null

  # TokenReview CRBs + the pod VAP binding are minted by Kyverno's
  # tenant-rolebinding-generator once the ns carries part-of=sycophant-tenant
  # (labelled in step_3, after the cluster chart installs the generator).

  # Kernel content lives at the convention path <hostPathBase>/<ns>/<workspace>;
  # the tenant install below sets hostPathBase to $HOME/sycophant/tmp (bind-
  # mounted into the node), delivered on the workspace's read-only kernel PV and
  # mounted read-only on the harness pod at /etc/kernels/hello-world, which reads
  # it in-process.
  mkdir -p "$HOME/sycophant/tmp/$NAMESPACE/hello-world"
  cp "$REPO_ROOT/examples/kernel/simple/AGENTS.md" "$HOME/sycophant/tmp/$NAMESPACE/hello-world/AGENTS.md"
  cp -r "$REPO_ROOT/examples/kernel/simple/agents" "$HOME/sycophant/tmp/$NAMESPACE/hello-world/agents"
  # The agent's `model:` frontmatter is the only turn model selector (the
  # harness reads it fresh each turn). The shared example defaults to an external
  # provider, so route this run at the in-cluster model under test.
  local agent_md="$HOME/sycophant/tmp/$NAMESPACE/hello-world/AGENTS.md"
  sed "s/^model:.*/model: ${INFERENCE_PROFILE}/" "$agent_md" > "$agent_md.tmp" && mv "$agent_md.tmp" "$agent_md"

  kubectl create secret generic sycophant-llm-openrouter -n "$NAMESPACE" \
    --from-literal=sycophant-llm-openrouter="$OPENROUTER_API_KEY" --dry-run=client -o yaml | kubectl apply -f - >/dev/null

  kubectl apply -f "$REPO_ROOT/examples/toolsets/ssh-credentials/fixtures/" -n "$NAMESPACE" >/dev/null
  ok "Namespace, RBAC, kernels, secrets, toolset fixtures applied"
}

# ---- step 3: deploy ----
step_3_deploy() {
  step "Step 3: Deploy charts"

  # Create the PSA-restricted release namespace before helm installs into it
  # (helm --create-namespace would land it bare). Same manifest `syco setup` applies.
  kubectl apply -f "$REPO_ROOT/charts/sycophant-cluster/system-ns.yaml" >/dev/null
  helm upgrade --install sycophant "$REPO_ROOT/charts/sycophant-cluster/" \
    -n sycophant-system --set policyEngine=kyverno --wait >/dev/null
  ok "Cluster chart installed"

  # Labelling the ns triggers the (label-matched) tenant-rolebinding-generator,
  # which mints the per-tenant TokenReview CRBs + the pod VAP binding. Kyverno
  # generate is async, so wait for the wiring before the controllers need it.
  kubectl label namespace "$NAMESPACE" app.kubernetes.io/part-of=sycophant-tenant --overwrite >/dev/null

  local crb
  for crb in toolset relay; do
    wait_for "${NAMESPACE}-${crb}-tokenreview CRB" 120 \
      "kubectl get clusterrolebinding ${NAMESPACE}-${crb}-tokenreview >/dev/null 2>&1"
  done
  wait_for "${NAMESPACE}-sycophant-pod-binding VAP binding" 120 \
    "kubectl get validatingadmissionpolicybinding ${NAMESPACE}-sycophant-pod-binding >/dev/null 2>&1"
  ok "Kyverno minted TokenReview CRBs + pod VAP binding (label-triggered)"

  # The pod VAP is now bound to this ns — assert it actually enforces. The
  # binding object existing does not mean the apiserver enforces it yet: there
  # is an eventual-consistency gap after Kyverno mints the binding, so poll the
  # probe until it is denied rather than asserting once. This pod is
  # PSA-restricted-compliant but sets automountServiceAccountToken: true, which
  # only the VAP forbids (PSA does not check it), so a denial citing the policy
  # proves the binding is live.
  local vap_probe_err vap_deadline
  vap_deadline=$((SECONDS + 30))
  while :; do
    vap_probe_err="$(kubectl apply -n "$NAMESPACE" -f - 2>&1 <<'POD' || true
apiVersion: v1
kind: Pod
metadata:
  name: vap-probe
  labels:
    app.kubernetes.io/part-of: sycophant
    app.kubernetes.io/component: toolset-ctrl
spec:
  automountServiceAccountToken: true
  securityContext:
    runAsNonRoot: true
    seccompProfile: { type: RuntimeDefault }
  containers:
    - name: c
      image: registry.k8s.io/pause:3.9
      securityContext:
        allowPrivilegeEscalation: false
        readOnlyRootFilesystem: true
        runAsNonRoot: true
        capabilities: { drop: ["ALL"] }
POD
)"
    kubectl delete pod vap-probe -n "$NAMESPACE" --ignore-not-found >/dev/null 2>&1 || true
    if printf '%s' "$vap_probe_err" | grep -qe 'cluster-gvisor-pod-policy' -e 'automountServiceAccountToken must be set to false'; then
      ok "pod VAP enforced (automountServiceAccountToken denied by cluster-gvisor-pod-policy)"
      break
    fi
    if [ "$SECONDS" -ge "$vap_deadline" ]; then
      warn "pod VAP did NOT deny the probe after 30s (binding not enforcing): ${vap_probe_err}"
      exit 1
    fi
    sleep 2
  done

  # Resolve local-registry digests for toolset images. The host rewrite
  # (localhost:5555 → sycophant-registry:5000) swaps the docker-push-facing
  # host for the in-cluster name resolved via the CoreDNS NodeHosts entry
  # added in patch_coredns_for_registry; the digest is identical either way.
  local git_ref ssh_ref stdlib_ref
  git_ref="$(docker inspect --format '{{range .RepoDigests}}{{println .}}{{end}}' localhost:5555/toolset-git:latest | grep '^localhost:5555/')"
  ssh_ref="$(docker inspect --format '{{range .RepoDigests}}{{println .}}{{end}}' localhost:5555/toolset-ssh-credentials:latest | grep '^localhost:5555/')"
  stdlib_ref="$(docker inspect --format '{{range .RepoDigests}}{{println .}}{{end}}' localhost:5555/toolset:latest | grep '^localhost:5555/')"
  git_ref="${git_ref/localhost:5555/sycophant-registry:5000}"
  ssh_ref="${ssh_ref/localhost:5555/sycophant-registry:5000}"
  stdlib_ref="${stdlib_ref/localhost:5555/sycophant-registry:5000}"

  # Kernel delivery is chart-values-driven — no Kernel CR. The per-workspace
  # read-only kernel PV renders from `.Values.workspaces` + hostPathBase; the
  # harness mounts it read-only and reads it in-process.

  # Readiness is gated by the install-wait post-install hook (helm waits for
  # hooks regardless of --wait), so native --wait is omitted here.
  # hostPathBase points at the bind-mounted node dir; content lives at
  # <base>/<ns>/<workspace> and surfaces on the harness at /etc/kernels/<workspace>.
  helm upgrade --install "$NAMESPACE" "$REPO_ROOT/charts/sycophant-tenant/" \
    -n "$NAMESPACE" \
    -f "$REPO_ROOT/docs/e2e/values.yaml" \
    --set-string "harness.kernels.hostPathBase=${HOME}/sycophant/tmp" \
    --set-string "toolsets.stdlib.image=${stdlib_ref}" \
    --set-string "toolsets.workspace-ro.image=${git_ref}" \
    --set-string "toolsets.ssh-credentials.image=${ssh_ref}" \
    --timeout=5m \
    >/dev/null
  ok "Tenant chart installed (client: ${CLIENT_NAME})"

  step_3_headscale_authkey

  # Grant rows are runtime data, not chart config: the operator writes them
  # into the chart-created `relay-grants` ConfigMap. The identity IS the code, and
  # the operator invents it — the relay mints nothing.
  kubectl patch configmap relay-grants -n "$NAMESPACE" --type=merge -p "$(
    cat <<EOF
{"data":{"${CLIENT_NAME}":"channel: app\nidentity: ${GRANT_CODE}\nworkspace: hello-world\n"}}
EOF
  )" >/dev/null
  ok "Grant row ${CLIENT_NAME} written (channel app, workspace hello-world)"

  # The fail-closed baseline, the per-profile egress CNP, and the per-grant
  # egress CNP are all chart-rendered — the structural proof that egress
  # authoring lives OUTSIDE the tenant. `relay-ingress` is the ONE object
  # carrying every relay ingress rule; its whole-object absence is the only way
  # the relay fails open, so the run hard-fails on it.
  # toolset-local is the in-cluster profile's egress hole (the prompt job's
  # toEndpoints rule to the inference pod); inference-local is that pod's own
  # ingress+DNS fence. Both render from the `local` inference entry.
  for cnp in tool-job-baseline toolset-deepseek-v4-flash toolset-local inference-local toolset-grant-hello-world-ssh-credentials-github relay-ingress; do
    if kubectl get ciliumnetworkpolicy "$cnp" -n "$NAMESPACE" >/dev/null 2>&1; then
      ok "CNP present: $cnp"
    else
      warn "expected CNP missing: $cnp"
      exit 1
    fi
  done

  # Every rendered workload pod must clear the pod VAP, the inference server's
  # weights-copy init container included. A VAP denial does not fail the helm
  # install: the Deployment object is created, but its ReplicaSet cannot create
  # the pod and emits a FailedCreate event citing the policy. So assert the
  # inference pod was admitted (a Pod object exists) and that no ReplicaSet in
  # the namespace reports an admission denial.
  wait_for "inference-${INFERENCE_PROFILE} pod admitted" 60 \
    "kubectl get pod -n '$NAMESPACE' -l app.kubernetes.io/component=inference,app.kubernetes.io/name=${INFERENCE_PROFILE} -o name 2>/dev/null | grep -q ." \
    || { warn "no pod for inference-${INFERENCE_PROFILE} — the ReplicaSet could not create one (admission?)"; \
         kubectl get events -n "$NAMESPACE" --field-selector reason=FailedCreate 2>/dev/null | tail -5 >&2; exit 1; }
  if kubectl get events -n "$NAMESPACE" --field-selector reason=FailedCreate 2>/dev/null \
       | grep -qiE 'denied|policy|automountServiceAccountToken'; then
    warn "a workload pod was denied admission by the pod VAP:"
    kubectl get events -n "$NAMESPACE" --field-selector reason=FailedCreate 2>/dev/null \
      | grep -iE 'denied|policy|automountServiceAccountToken' | sed 's/^/    /' >&2
    exit 1
  fi
  ok "All rendered workload pods admitted (inference copy-path init container included)"
}

# Stand the app adapter's tailnet identity up: create the headscale user,
# mint a pre-auth key, and hand it to the adapter through its authKey Secret.
# headscale 0.28+ requires a numeric user id for `-u`, hence the lookup.
step_3_headscale_authkey() {
  step "Minting a headscale pre-auth key for the app adapter"
  wait_for "headscale Available" 180 \
    "kubectl wait --for=condition=Available --timeout=5s -n '$NAMESPACE' deploy/headscale"

  kubectl exec -n "$NAMESPACE" deploy/headscale -- \
    headscale users create "$HEADSCALE_USER" >/dev/null 2>&1 || true
  local user_id
  user_id="$(kubectl exec -n "$NAMESPACE" deploy/headscale -- \
    headscale users list -o json | jq -r ".[] | select(.name==\"$HEADSCALE_USER\") | .id")"
  [ -n "$user_id" ] || { warn "headscale user $HEADSCALE_USER has no id"; return 1; }

  # The key never reaches a printf/echo: it goes straight from the mint
  # command into kubectl's stdin.
  kubectl exec -n "$NAMESPACE" deploy/headscale -- \
    headscale preauthkeys create -u "$user_id" -e 24h | tail -1 | tr -d '\r\n' \
    | kubectl create secret generic "$ADAPTER_AUTHKEY_SECRET" -n "$NAMESPACE" \
        --from-file=authkey=/dev/stdin --dry-run=client -o yaml \
    | kubectl apply -n "$NAMESPACE" -f - >/dev/null
  ok "Pre-auth key stored in Secret ${ADAPTER_AUTHKEY_SECRET}"

  wait_for "app adapter Available" 180 \
    "kubectl wait --for=condition=Available --timeout=5s -n '$NAMESPACE' deploy/adapter-app"
  ok "App adapter Available"
}

# ---- step 4: verify chart ----
step_4_verify() {
  step "Step 4: Verify chart"

  # Per-workspace harness Deployment rendered by the chart. Wait on
  # the Deployment becoming Available rather than a specific pod name,
  # since the pod name now carries a ReplicaSet suffix.
  kubectl wait -n "$NAMESPACE" --for=condition=Available --timeout=180s \
    deployment/hello-world >/dev/null
  ok "hello-world workspace Ready"

  # The in-cluster inference server must reach Available and stay healthy: the
  # server surviving is the non-racy guarantee this run owns. It is the slowest
  # workload — the copy init container writes the multi-gigabyte GGUF into the
  # emptyDir, then the server memory-maps it on CPU — so the timeout is generous.
  kubectl wait -n "$NAMESPACE" --for=condition=Available --timeout=600s \
    "deployment/inference-${INFERENCE_PROFILE}" >/dev/null
  ok "in-cluster inference server (${INFERENCE_PROFILE}) Available"

  # Workspace-init Job must COMPLETE: it binds the workspace PVC (first
  # consumer under WaitForFirstConsumer) and establishes the git baseline
  # for every workspace. helm --wait gates on PVC Bound, not Job completion,
  # so a failed git baseline would otherwise pass silently — assert it here.
  if kubectl wait -n "$NAMESPACE" --for=condition=complete --timeout=120s \
       job -l app.kubernetes.io/component=workspace-init,app.kubernetes.io/name=hello-world >/dev/null 2>&1; then
    ok "hello-world workspace-init Job complete (PVC bound + git baseline)"
  else
    warn "hello-world workspace-init Job did not complete"
    kubectl get job,pod -n "$NAMESPACE" -l app.kubernetes.io/component=workspace-init 2>&1 | sed 's/^/    /' >&2
    return 1
  fi

  # The stdlib toolset must be in the rendered toolset-config ConfigMap by now.
  # Toolset pods are spawned lazily on the first CallTool RPC, so zero pods is
  # the correct pre-tool-call state. Step 6 verifies the pod appears after the
  # first call and survives subsequent calls (keepalive=true).
  if ! kubectl get configmap toolset-config -n "$NAMESPACE" \
       -o jsonpath='{.data.toolsets\.yaml}' 2>/dev/null | grep -q '^stdlib:'; then
    warn "toolset-config ConfigMap has no 'stdlib' entry — helm render failed"
    return 1
  fi
  ok "toolset-config carries the stdlib toolset (keepalive=true, lazy-spawn)"
}

# ---- step 5: flutter ----
# Forwards headscale's HTTP API, so the operator can point the host's
# Tailscale at the in-cluster control plane and reach the app adapter over
# the tailnet. This is the only forward the run stands up: the relay's app
# port stays unreachable from the host, so a client can only arrive through
# the adapter, which is the path the assertions are meant to cover.
#
# Self-healing loop: kubectl port-forward binds to one pod's stream and dies
# on pod replacement (rollout, eviction, crash). The loop reconnects to the
# deployment's current pod automatically. Disable errexit inside the subshell
# so a non-zero kubectl exit doesn't kill the loop. The trap ensures the
# kubectl child dies with the subshell — without it the kubectl process would
# be orphaned and survive script teardown.
step_5_headscale_port_forward() {
  ( set +e
    kpid=""
    trap '[ -n "$kpid" ] && kill "$kpid" 2>/dev/null; exit' TERM INT EXIT
    while true; do
      kubectl port-forward -n "$NAMESPACE" svc/headscale 8080:8080 --address 0.0.0.0 \
        >/dev/null 2>&1 &
      kpid=$!
      wait "$kpid"
      sleep 2
    done ) &
  CLEANUP_PIDS+=($!)
  HEADSCALE_FORWARD_STARTED=1
}

# The code is read back from the row this script itself wrote. Once a device
# has redeemed it the row is spent, and a second presentation is refused —
# so an already-redeemed run surfaces no code.
step_5_grant_code() {
  if kubectl get secret relay-registered-keys -n "$NAMESPACE" \
       -o jsonpath="{.data.${CLIENT_NAME}}" 2>/dev/null | grep -q .; then
    ok "Grant row ${CLIENT_NAME} already redeemed — reusing the registered key" >&2
    printf ''
    return 0
  fi
  kubectl get configmap relay-grants -n "$NAMESPACE" \
    -o jsonpath="{.data.${CLIENT_NAME}}" \
    | sed -n 's/^identity: //p' | tr -d '\n'
}

step_5_flutter_macos() {
  local code="$1"
  step "Building Flutter macOS app"
  # CODE_SIGNING_ALLOWED=NO: build unsigned. arm64 applies a linker ad-hoc
  # signature so the app still launches locally; no Apple team or
  # provisioning profile needed. Never re-enable signing for the e2e.
  ( cd "$REPO_ROOT/client" && FLUTTER_XCODE_CODE_SIGNING_ALLOWED=NO flutter build macos --debug ) >/tmp/sycophant-flutter-build.log 2>&1 \
    || { warn "flutter build macos --debug failed; see /tmp/sycophant-flutter-build.log"; return 1; }
  local app_path="$REPO_ROOT/client/build/macos/Build/Products/Debug/sycophant.app"
  ok "macOS app built: $app_path"

  step "Launching macOS app"
  pkill -f "sycophant.app/Contents/MacOS/sycophant" 2>/dev/null || true
  sleep 1
  # `open -n` forces a new instance and bypasses the LaunchServices
  # "bring to front" path that returns -600 on stale state.
  if ! open -n "$app_path" 2>/dev/null; then
    warn "open -n failed, executing binary directly"
    "$app_path/Contents/MacOS/sycophant" >/dev/null 2>&1 &
  fi

  if [ -n "$code" ]; then
    printf '\n\033[1;35m========== Paste these into the app ==========\033[0m\n'
    printf '  Server:           %s\n' "$TAILNET_RELAY_ADDR"
    printf '  Workspace:        hello-world\n'
    printf '  Grant code:       %s\n' "$code"
    printf '  In-cluster model: %s  (a turn requesting this model routes to the inference-%s Service)\n' "$INFERENCE_PROFILE" "$INFERENCE_PROFILE"
    printf '\033[1;35m===============================================\033[0m\n'
    printf 'Join the tailnet first (headscale is port-forwarded on :8080):\n'
    printf '  sudo tailscale up --login-server=http://localhost:8080 --auth-key=<key>\n'
    printf 'Mint the key with:\n'
    printf '  kubectl exec -n %s deploy/headscale -- headscale preauthkeys create -u <id> -e 24h\n' "$NAMESPACE"
    printf 'If the app opens at the chat screen with stale credentials, tap Sign Out first.\n'
  else
    printf '\n\033[1;35m========== App already enrolled ==========\033[0m\n'
    printf '  Just send the chat message below.\n'
    printf '\033[1;35m==========================================\033[0m\n'
  fi
}

# Backend-only (FLUTTER_TARGET=none): no local client to launch. Bring up the
# external listener, surface the connect details, and pause for the operator to
# attach a client from another machine (e.g. over Tailscale) and drive the
# Step 6 chat. Step 6 still runs afterward.
step_5_backend_only() {
  step "Step 5: Backend-only — connect a remote client"

  step_5_headscale_port_forward
  local code
  code="$(step_5_grant_code)"

  local addr="$TAILNET_RELAY_ADDR"

  printf '\n  Backend is up. Connect a client from another machine:\n' >&2
  printf '    Server:          %s\n' "$addr" >&2
  printf '    Workspace:       hello-world\n' >&2
  printf '    Namespace:       %s\n' "$NAMESPACE" >&2
  printf '    Client:          %s\n' "$CLIENT_NAME" >&2
  printf '    Grant code:      %s\n' "$code" >&2
  printf '    In-cluster model:%s  (routes to the inference-%s Service)\n' "$INFERENCE_PROFILE" "$INFERENCE_PROFILE" >&2

  pause "From the other machine, point the app at ${addr}, enroll with the code
   above, then send these three messages IN ORDER, ONE tool per message (the
   small in-cluster model calls a single tool reliably, not a chained sequence):
     1. (chip 'ssh-credentials: demo-key')  Use the test-cmd tool.
     2. (chip 'ssh-credentials: demo-key')  Use the Shell tool to run \`dmesg | head -1\`.
     3. (chip 'ssh-credentials: github')    Use the test-cred tool.
   The test-cmd reply's tool-result card shows the credential REDACTED. Step 6
   asserts on the toolset tool + the stdlib pod the Shell call triggers — same
   as the local-client path."
}

step_5_flutter() {
  if [ "$FLUTTER_TARGET" = "none" ]; then
    step_5_backend_only
    return
  fi

  step "Step 5: Flutter ${FLUTTER_TARGET} + chat"

  step_5_headscale_port_forward
  local code
  code="$(step_5_grant_code)"

  case "$FLUTTER_TARGET" in
    macos)   step_5_flutter_macos "$code" ;;
  esac

  pause "Tap Enroll, then drive the grant flow IN ORDER, ONE tool per message
   (the in-cluster model is small and calls a single tool reliably, not a
   chained sequence):
   1. Select the credential chip 'ssh-credentials: demo-key' above the
      input box, then send EXACTLY:
        Use the test-cmd tool.
      test-cmd runs under the demo-key grant (key delivered at its ssh
      path). The reply's tool-result card must show the key REDACTED, not
      raw — that card is the scrub proof, independent of the model's prose.
   2. Keep 'demo-key' selected and send EXACTLY:
        Use the Shell tool to run \`dmesg | head -1\`.
      Shell spawns the stdlib pod Step 6 asserts on for gVisor + egress +
      credential isolation and keepalive.
   3. After the reply lands, deselect 'demo-key', select
      'ssh-credentials: github', then send EXACTLY:
        Use the test-cred tool.
      test-cred reads the pathless grant at the convention target. Step 6
      asserts that pod's grant label and credential file, so send this
      within the keepalive window (10 min) of step 1."
}

# ---- step 6: security assertions ----
step_6_security() {
  step "Step 6: Security assertions"

  # Prove the driven turn ran on the in-cluster model before any assertion that
  # depends on the model competently calling a tool, so a routing failure is
  # never masked by a tool-calling failure.
  step_6_inference_agent_turn

  # Wait for the per-workspace stdlib toolset pod (lazy-spawned by
  # toolset-ctrl on the first stdlib Bash/ReadFile/WriteFile/ListDirectory
  # call from the agent). 90s buffer accounts for the known ARM64 gVisor
  # `epoll_pwait` slow path on first cold start — see vault
  # `sycophant-kernel-isolation-runtime`.
  local toolset_selector="app.kubernetes.io/component=tool-job,sycophant.md/workspace=hello-world,sycophant.md/toolset=stdlib"
  local task_pod
  wait_for "stdlib toolset pod for hello-world" 90 \
    "kubectl get pod -n '$NAMESPACE' -l '$toolset_selector' -o name 2>/dev/null | grep -q ."
  task_pod="$(kubectl get pod -n "$NAMESPACE" \
                -l "$toolset_selector" \
                -o jsonpath='{.items[0].metadata.name}')"
  kubectl wait -n "$NAMESPACE" --for=condition=Ready --timeout=60s "pod/$task_pod" >/dev/null
  ok "stdlib toolset pod Ready ($task_pod)"

  local first_line
  first_line="$(kubectl exec -n "$NAMESPACE" "$task_pod" -- dmesg 2>/dev/null | head -1)"
  if echo "$first_line" | grep -q 'Starting gVisor'; then
    ok "gVisor kernel isolation"
  else
    warn "gVisor first dmesg line was: $first_line"
    return 1
  fi

  # Scan for real API-key prefixes in two sinks:
  #   1. harness stdout (kubectl logs)
  #   2. conversation log files on the harness's conversation-data PVC
  # Patterns match a prefix + length floor — `sk-ant-` + 50+ base64 chars
  # for Anthropic, `sk-` + 40+ for generic OpenAI-style. The length floor
  # avoids false positives on the bare strings "sk-" or "sk-ant-" appearing
  # in normal text.
  local key_regex='sk-ant-[A-Za-z0-9_-]{50,}|sk-[A-Za-z0-9_-]{40,}'

  local harness_hits
  harness_hits="$(kubectl logs -n "$NAMESPACE" deployment/hello-world -c harness --tail=10000 2>/dev/null \
                        | grep -cE "$key_regex" || true)"

  # The conversation log is on the harness's OWN RWO PVC
  # (<ws>-conversation-data at /var/lib/harness/conversations). A separate
  # pod can't mount an RWO PVC, and the harness image is FROM scratch (no
  # shell), so attach an ephemeral busybox to the harness pod sharing its
  # PID namespace and read the dir via /proc/1/root. (Fallback if a hardened
  # node blocks /proc/1/root via ptrace_scope: scale the harness to 0,
  # mount <ws>-conversation-data RO in a probe pod, grep, then scale back to 1.)
  local tb_pod scrub_c patch
  tb_pod="$(kubectl get pod -n "$NAMESPACE" \
    -l app.kubernetes.io/component=harness,sycophant.md/workspace=hello-world \
    -o jsonpath='{.items[0].metadata.name}')"
  scrub_c="syco-scrub-$$"
  patch='{"spec":{"ephemeralContainers":[{"name":"'"$scrub_c"'","image":"busybox:1.36","command":["sleep","180"],"targetContainerName":"harness","securityContext":{"runAsNonRoot":true,"runAsUser":1000,"readOnlyRootFilesystem":true,"allowPrivilegeEscalation":false,"capabilities":{"drop":["ALL"]},"seccompProfile":{"type":"RuntimeDefault"}}}]}}'
  kubectl patch pod "$tb_pod" -n "$NAMESPACE" \
    --subresource=ephemeralcontainers --type=strategic -p "$patch" >/dev/null
  local conv_hits=""
  for _ in $(seq 1 15); do
    conv_hits="$(kubectl exec -n "$NAMESPACE" "$tb_pod" -c "$scrub_c" -- \
      sh -c "grep -rcE '$key_regex' /proc/1/root/var/lib/harness/conversations 2>/dev/null | grep -v ':0\$' | wc -l" 2>/dev/null)" && break
    sleep 2
  done
  conv_hits="${conv_hits//[[:space:]]/}"
  conv_hits="${conv_hits:-0}"

  if [ "$harness_hits" -eq 0 ] && [ "$conv_hits" -eq 0 ]; then
    ok "Secret scrubbing (0 sk-ant-/sk- matches in harness + conv log)"
  else
    warn "Unscrubbed key prefixes detected: harness=$harness_hits conv_log=$conv_hits"
    return 1
  fi

  # The granted demo key: its raw bytes must never appear in the
  # conversation log, and the redaction marker must — together proving the
  # credential file was read end-to-end by the granted tool call and
  # scrubbed on the way out of the pod.
  local grant_raw grant_marker
  grant_raw="$(kubectl exec -n "$NAMESPACE" "$tb_pod" -c "$scrub_c" -- \
    sh -c "grep -rc 'FAKE-ED25519-PRIVATE-KEY' /proc/1/root/var/lib/harness/conversations 2>/dev/null | grep -v ':0\$' | wc -l" 2>/dev/null)"
  grant_marker="$(kubectl exec -n "$NAMESPACE" "$tb_pod" -c "$scrub_c" -- \
    sh -c "grep -rl 'REDACTED:demo-ssh-key' /proc/1/root/var/lib/harness/conversations 2>/dev/null | wc -l" 2>/dev/null)"
  grant_raw="${grant_raw//[[:space:]]/}"; grant_raw="${grant_raw:-1}"
  grant_marker="${grant_marker//[[:space:]]/}"; grant_marker="${grant_marker:-0}"
  if [ "$grant_raw" -eq 0 ] && [ "$grant_marker" -ge 1 ]; then
    ok "Grant credential read + scrubbed (marker present, raw bytes absent)"
  else
    warn "grant credential evidence wrong: raw_files=$grant_raw marker_files=$grant_marker"
    return 1
  fi

  # The harness persists tool execution to a single append-only
  # execution.json per conversation, under
  # /var/lib/harness/conversations/<ws>/<conv_id>/execution.json: one
  # ND-JSON record per ToolResultFrame (stdout / stderr / image, terminated by
  # one ToolComplete), each line carrying its call_id, with binary frames moved
  # to content-addressed blobs/sha256/<hex> in the same conversation dir. A tool
  # that ran to completion leaves a non-empty execution.json, so assert a
  # non-empty execution.json exists. Read it via the same ephemeral container
  # the conv-log scrub scan above used.
  local exec_ok=""
  for _ in $(seq 1 30); do
    exec_ok="$(kubectl exec -n "$NAMESPACE" "$tb_pod" -c "$scrub_c" -- \
      sh -c 'find /proc/1/root/var/lib/harness/conversations -type f -name "execution.json" -size +0c 2>/dev/null | head -1')" \
      && [ -n "$exec_ok" ] && break
    sleep 2
  done
  if [ -n "$exec_ok" ]; then
    ok "Tool execution (non-empty execution.json record persisted)"
  else
    warn "no execution.json record on harness PVC"
    return 1
  fi

  if kubectl exec -n "$NAMESPACE" "$task_pod" -- \
       wget -qO- --timeout=3 https://httpbin.org/ip >/dev/null 2>&1; then
    warn "stdlib toolset pod reached httpbin.org — NetworkPolicy egress NOT enforced"
    return 1
  else
    ok "NetworkPolicy blocks stdlib toolset egress"
  fi

  # L7 DNS allowlist holds: stdlib must NOT resolve arbitrary names (the DNS-tunnel
  # exfil guard — proves baseline + per-toolset CNP compose without L4-shadows-L7).
  # Best-effort: skip cleanly if the toolset image lacks nslookup.
  if kubectl exec -n "$NAMESPACE" "$task_pod" -- sh -c 'command -v nslookup' >/dev/null 2>&1; then
    if kubectl exec -n "$NAMESPACE" "$task_pod" -- nslookup example.com >/dev/null 2>&1; then
      warn "stdlib resolved example.com — L7 DNS allowlist NOT enforced"
      return 1
    else
      ok "L7 DNS allowlist blocks arbitrary name resolution"
    fi
  else
    warn "nslookup absent in toolset image — skipping L7 DNS probe (wget check still covers egress containment)"
  fi

  if kubectl exec -n "$NAMESPACE" "$task_pod" -- \
       cat /run/secrets/toolset/api-key >/dev/null 2>&1; then
    warn "/run/secrets/toolset/api-key exists inside stdlib toolset pod — credential leak"
    return 1
  else
    ok "Credential isolation (no LLM key in stdlib toolset pod)"
  fi

  # The stdlib toolset is bound bare, so this call resolved no grant. The
  # convention credential target must therefore be empty: a grant a call never
  # selected must not reach the pod.
  if kubectl exec -n "$NAMESPACE" "$task_pod" -- \
       cat /run/secrets/grant/credential >/dev/null 2>&1; then
    warn "/run/secrets/grant/credential exists inside a grantless toolset pod — credential leak"
    return 1
  else
    ok "Credential isolation (no grant credential in a grantless toolset pod)"
  fi

  if kubectl get serviceaccounts -n "$NAMESPACE" -l sycophant.md/type=workspace-sa -o name \
       | grep -q sa-hello-world; then
    ok "Workspace ServiceAccounts present"
  else
    warn "sa-hello-world ServiceAccount missing"
    return 1
  fi

  step_6_grant_credentials
  step_6_relay_sheds_tsnet
  step_6_adapter_isolation
  step_6_adapter_port_fence
  step_6_inference_fence
  step_6_grant_row_hot_reload
}

# The grant-bearing ssh-credentials pod: it must carry the grant label its
# egress policy selects on, and hold a readable credential at the convention
# target a pathless grant defaults to. Whether a grant change retires a live
# keepalive pod is decided in the controller and pinned there — active jobs are
# keyed per tool, so two different tools never contend for one pod and no
# arrangement of these two messages can exercise that decision.
step_6_grant_credentials() {
  local selector="app.kubernetes.io/component=tool-job,sycophant.md/workspace=hello-world,sycophant.md/toolset=ssh-credentials"
  local grant_pod
  wait_for "github-granted ssh-credentials pod" 60 \
    "kubectl get pod -n '$NAMESPACE' -l '$selector,sycophant.md/grant=github' -o name 2>/dev/null | grep -q ." \
    || { warn "no pod carries sycophant.md/grant=github"; return 1; }
  grant_pod="$(kubectl get pod -n "$NAMESPACE" -l "$selector,sycophant.md/grant=github" \
    -o jsonpath='{.items[0].metadata.name}')"

  if kubectl exec -n "$NAMESPACE" "$grant_pod" -- \
       grep -q 'FAKE-ED25519-PRIVATE-KEY' /run/secrets/grant/credential 2>/dev/null; then
    ok "Pathless grant delivers a readable credential at the convention target"
  else
    warn "credential absent or unreadable at /run/secrets/grant/credential in $grant_pod"
    return 1
  fi

}

# The tailnet terminus lives on the app adapter, not the relay. A tailscale
# container back in the relay pod would widen the relay's per-pod egress and
# let loopback bypass both the tailnet and the CNP.
step_6_relay_sheds_tsnet() {
  local images
  images="$(kubectl get deploy relay-ctrl -n "$NAMESPACE" \
    -o jsonpath='{range .spec.template.spec.containers[*]}{.name}={.image}{"\n"}{end}')"
  if printf '%s' "$images" | grep -qi 'tailscale\|tsnet'; then
    warn "relay pod carries a tailscale/tsnet container: $images"
    return 1
  fi
  ok "Relay pod carries no tailscale container"
}

# Every adapter runs under gVisor. It terminates a foreign protocol and
# parses whatever the outside world sends it.
step_6_adapter_isolation() {
  local rc
  rc="$(kubectl get pods -n "$NAMESPACE" \
    -l app.kubernetes.io/component=adapter \
    -o jsonpath='{.items[0].spec.runtimeClassName}')"
  if [ "$rc" != "gvisor" ]; then
    warn "app adapter pod runtimeClassName is '$rc', expected gvisor"
    return 1
  fi
  ok "App adapter runs under gVisor"
}

# An accept/deny pair. A deny-only probe passes on a fence that
# refuses everything, so both halves are required: the labelled pod must
# reach 9092 and the unlabelled one must not.
step_6_adapter_port_fence() {
  step "Adapter-port fence (9092)"
  local target="relay-ctrl.${NAMESPACE}.svc.cluster.local"

  adapter_probe() {
    local name="$1" class="$2"
    kubectl delete pod "$name" -n "$NAMESPACE" --ignore-not-found --wait=true >/dev/null 2>&1
    kubectl run "$name" -n "$NAMESPACE" --restart=Never --quiet \
      --image=busybox:1.36 \
      --labels="app.kubernetes.io/part-of=sycophant,app.kubernetes.io/component=adapter${class}" \
      --overrides='{"spec":{"automountServiceAccountToken":false,"runtimeClassName":"gvisor","containers":[{"name":"probe","image":"busybox:1.36","securityContext":{"runAsNonRoot":true,"runAsUser":65534,"readOnlyRootFilesystem":true,"allowPrivilegeEscalation":false,"capabilities":{"drop":["ALL"]},"seccompProfile":{"type":"RuntimeDefault"}},"command":["sh","-c","nc -z -w 5 '"$target"' 9092"]}]}}' \
      >/dev/null 2>&1
    kubectl wait --for=jsonpath='{.status.phase}'=Succeeded pod/"$name" \
      -n "$NAMESPACE" --timeout=60s >/dev/null 2>&1
  }

  if adapter_probe adapter-probe-allow ",sycophant.md/adapter-class=principal"; then
    ok "principal-labelled pod reaches the adapter port"
  else
    warn "principal-labelled pod could NOT reach the adapter port — the fence admits nothing"
    kubectl logs adapter-probe-allow -n "$NAMESPACE" 2>&1 | tail -5 || true
    kubectl delete pod adapter-probe-allow adapter-probe-deny -n "$NAMESPACE" --ignore-not-found >/dev/null 2>&1
    return 1
  fi

  if adapter_probe adapter-probe-deny ""; then
    warn "a pod without adapter-class=principal reached the adapter port — the fence is open"
    kubectl delete pod adapter-probe-allow adapter-probe-deny -n "$NAMESPACE" --ignore-not-found >/dev/null 2>&1
    return 1
  else
    ok "pod without adapter-class=principal is refused on the adapter port"
  fi

  kubectl delete pod adapter-probe-allow adapter-probe-deny -n "$NAMESPACE" --ignore-not-found >/dev/null 2>&1
}

# The in-cluster inference Service, fenced two ways.
#
# L3/L4: its ingress admits only pods carrying the profile's toolset label, so a
# labelled probe connects and an unlabelled one is refused. Both halves are
# required — a deny-only probe passes on a fence that admits nothing — so this is
# the same accept/deny pair as the adapter fence.
#
# L7: from a pod that DOES carry the label (L3/L4 already open), the Cilium HTTP
# allowlist answers /metrics and /slots with 403 and lets a completion through
# with 200. A denial here is a 403, not a drop, so the 403 is the positive signal
# the filter is engaged; a 200 on /metrics would mean a policy applied from
# outside the chart shadowed it, which the rendered-manifest checks cannot see.
# The completion 200 is the accept half and is itself the proof a turn reaches
# the in-cluster server rather than an external provider, since the probe dials
# the cluster Service by name. First-token latency, sustained tokens/sec and the
# pod's memory high-water mark are recorded from the server's own response
# timings and cgroup; they are informational and never fail the run.
step_6_inference_fence() {
  step "In-cluster inference fence (${INFERENCE_PROFILE})"
  local svc="inference-${INFERENCE_PROFILE}.${NAMESPACE}.svc.cluster.local"

  # --- L3/L4: reachable only with the profile's toolset label ---
  inference_l4_probe() {
    local name="$1" extra="$2"
    kubectl delete pod "$name" -n "$NAMESPACE" --ignore-not-found --wait=true >/dev/null 2>&1
    kubectl run "$name" -n "$NAMESPACE" --restart=Never --quiet \
      --image=busybox:1.36 \
      --labels="app.kubernetes.io/part-of=sycophant${extra}" \
      --overrides='{"spec":{"automountServiceAccountToken":false,"containers":[{"name":"probe","image":"busybox:1.36","securityContext":{"runAsNonRoot":true,"runAsUser":65534,"readOnlyRootFilesystem":true,"allowPrivilegeEscalation":false,"capabilities":{"drop":["ALL"]},"seccompProfile":{"type":"RuntimeDefault"}},"command":["sh","-c","nc -z -w 5 '"$svc"' '"$INFERENCE_PORT"'"]}]}}' \
      >/dev/null 2>&1
    kubectl wait --for=jsonpath='{.status.phase}'=Succeeded pod/"$name" \
      -n "$NAMESPACE" --timeout=60s >/dev/null 2>&1
  }

  if inference_l4_probe inference-probe-allow ",sycophant.md/toolset=${INFERENCE_PROFILE}"; then
    ok "toolset-labelled pod reaches the inference Service"
  else
    warn "toolset-labelled pod could NOT reach the inference Service — the fence admits nothing"
    kubectl logs inference-probe-allow -n "$NAMESPACE" 2>&1 | tail -5 || true
    kubectl delete pod inference-probe-allow inference-probe-deny -n "$NAMESPACE" --ignore-not-found >/dev/null 2>&1
    return 1
  fi

  if inference_l4_probe inference-probe-deny ""; then
    warn "a pod without the profile's toolset label reached the inference Service — the fence is open"
    kubectl delete pod inference-probe-allow inference-probe-deny -n "$NAMESPACE" --ignore-not-found >/dev/null 2>&1
    return 1
  else
    ok "pod without the profile's toolset label is refused on the inference Service"
  fi
  kubectl delete pod inference-probe-allow inference-probe-deny -n "$NAMESPACE" --ignore-not-found >/dev/null 2>&1

  # --- L7: the HTTP allowlist closes every route but the two completions ---
  # One labelled pod probes /metrics, /slots and a completion, printing a status
  # line per route plus the server's response so the host can parse it. The pod
  # carries the profile's toolset label, so toolset-<key> grants it egress to the
  # inference endpoint and DNS for the Service FQDN.
  kubectl delete pod inference-l7-probe -n "$NAMESPACE" --ignore-not-found --wait=true >/dev/null 2>&1
  kubectl apply -n "$NAMESPACE" -f - >/dev/null <<POD
apiVersion: v1
kind: Pod
metadata:
  name: inference-l7-probe
  labels:
    app.kubernetes.io/part-of: sycophant
    sycophant.md/toolset: ${INFERENCE_PROFILE}
spec:
  restartPolicy: Never
  automountServiceAccountToken: false
  securityContext:
    runAsNonRoot: true
    runAsUser: 65534
    seccompProfile: { type: RuntimeDefault }
  containers:
    - name: probe
      image: busybox:1.36
      securityContext:
        allowPrivilegeEscalation: false
        readOnlyRootFilesystem: true
        runAsNonRoot: true
        capabilities: { drop: ["ALL"] }
      command: ["sh", "-c"]
      args:
        - |
          b=http://${svc}:${INFERENCE_PORT}
          code() { wget -S -q -O /dev/null "\$1" 2>&1 | grep -o 'HTTP/[0-9.]* [0-9][0-9][0-9]' | tail -1 | grep -o '[0-9][0-9][0-9]\$'; }
          echo "METRICS=\$(code \$b/metrics)"
          echo "SLOTS=\$(code \$b/slots)"
          r=\$(wget -S -q -O- --header 'Content-Type: application/json' --post-data '{"model":"${INFERENCE_PROFILE}","messages":[{"role":"user","content":"Reply with a short greeting."}],"stream":false,"max_tokens":32}' "\$b/v1/chat/completions" 2>&1 || true)
          echo "COMPLETION=\$(printf '%s' "\$r" | grep -o 'HTTP/[0-9.]* [0-9][0-9][0-9]' | tail -1 | grep -o '[0-9][0-9][0-9]\$')"
          printf '%s' "\$r" | tr ',{}' '\n' | grep -oE '"(completion_tokens|prompt_ms|predicted_ms|predicted_per_second)":[0-9.]+'
          t=\$(wget -S -q -O- --header 'Content-Type: application/json' --post-data '{"model":"${INFERENCE_PROFILE}","messages":[{"role":"user","content":"What is the weather in Paris right now? You must call the get_weather tool to answer."}],"tools":[{"type":"function","function":{"name":"get_weather","description":"Get the current weather for a city.","parameters":{"type":"object","properties":{"city":{"type":"string"}},"required":["city"]}}}],"tool_choice":"auto","stream":false}' "\$b/v1/chat/completions" 2>&1 || true)
          echo "TOOLCALL=\$(printf '%s' "\$t" | grep -o 'HTTP/[0-9.]* [0-9][0-9][0-9]' | tail -1 | grep -o '[0-9][0-9][0-9]\$')"
          if printf '%s' "\$t" | grep -qE '"tool_calls":[[:space:]]*\[[[:space:]]*\{'; then echo "TOOLCALLS=present"; else echo "TOOLCALLS=absent"; fi
POD
  if ! kubectl wait --for=jsonpath='{.status.phase}'=Succeeded pod/inference-l7-probe \
         -n "$NAMESPACE" --timeout=120s >/dev/null 2>&1; then
    warn "inference L7 probe did not complete"
    kubectl logs inference-l7-probe -n "$NAMESPACE" 2>&1 | tail -10 || true
    kubectl delete pod inference-l7-probe -n "$NAMESPACE" --ignore-not-found >/dev/null 2>&1
    return 1
  fi
  local out metrics_code slots_code completion_code toolcall_code toolcalls
  out="$(kubectl logs inference-l7-probe -n "$NAMESPACE" 2>/dev/null)"
  kubectl delete pod inference-l7-probe -n "$NAMESPACE" --ignore-not-found >/dev/null 2>&1
  metrics_code="$(printf '%s' "$out" | sed -n 's/^METRICS=//p')"
  slots_code="$(printf '%s' "$out" | sed -n 's/^SLOTS=//p')"
  completion_code="$(printf '%s' "$out" | sed -n 's/^COMPLETION=//p')"
  toolcall_code="$(printf '%s' "$out" | sed -n 's/^TOOLCALL=//p')"
  toolcalls="$(printf '%s' "$out" | sed -n 's/^TOOLCALLS=//p')"

  if [ "$metrics_code" = "403" ] && [ "$slots_code" = "403" ]; then
    ok "L7 allowlist denies /metrics and /slots with 403 (filter engaged)"
  else
    warn "L7 allowlist NOT engaged: /metrics=${metrics_code:-none} /slots=${slots_code:-none} — a 200 means a policy elsewhere shadowed the filter"
    return 1
  fi

  if [ "$completion_code" = "200" ]; then
    ok "In-cluster turn served: completion returned 200 from the inference pod (not an external provider)"
  else
    warn "completion did not return 200 from the inference Service: got ${completion_code:-none}"
    return 1
  fi

  # A tools request that must trigger a call. Both halves in one assertion: a
  # missing --chat-template-file file crashes the server (status != 200) and a
  # broken embedded template silently drops the tools array (tool_calls empty or
  # null → absent). A corrected template returns 200 with a non-empty tool_calls.
  if [ "$toolcall_code" = "200" ] && [ "$toolcalls" = "present" ]; then
    ok "Tool-call turn served: the model returned a non-empty tool_calls with 200 (the chat template renders the tools)"
  else
    warn "tool-call turn failed: status=${toolcall_code:-none} tool_calls=${toolcalls:-none} — a broken or dropped chat template drops the tools array (empty/null tool_calls); a missing template file crashes the server (status != 200)"
    return 1
  fi

  # Informational: the server reports its own timings in the response, so read
  # first-token (prefill) latency and sustained rate from there; read the memory
  # high-water from the pod's cgroup. None of these fail the run.
  local prompt_ms rate mem
  prompt_ms="$(printf '%s' "$out" | sed -n 's/.*"prompt_ms":\([0-9.]*\).*/\1/p' | head -1)"
  rate="$(printf '%s' "$out" | sed -n 's/.*"predicted_per_second":\([0-9.]*\).*/\1/p' | head -1)"
  mem="$(kubectl exec -n "$NAMESPACE" "deploy/inference-${INFERENCE_PROFILE}" -- \
    cat /sys/fs/cgroup/memory.peak 2>/dev/null | tr -d '[:space:]' || true)"
  printf '   inference metrics: first-token(prefill)=%sms  sustained=%s tok/s  mem-peak=%s bytes\n' \
    "${prompt_ms:-n/a}" "${rate:-n/a}" "${mem:-n/a}"
}

# The fence above proves the Service and its network policy; this proves the
# agent actually ran on it. The harness reads `model: <profile>` from AGENTS.md
# each turn and asks the controller for a matching prompt job, so a
# `toolset-prompt-<profile>` Job in the controller log is direct evidence the
# driven conversation routed to the in-cluster model, not an external provider.
step_6_inference_agent_turn() {
  step "Agent turn ran on the in-cluster model (${INFERENCE_PROFILE})"
  if kubectl logs -n "$NAMESPACE" deploy/toolset-ctrl 2>/dev/null \
       | grep -q "toolset-prompt-${INFERENCE_PROFILE}-"; then
    ok "harness routed a turn to inference-${INFERENCE_PROFILE} (prompt Job spawned)"
  else
    warn "no toolset-prompt-${INFERENCE_PROFILE} Job — the driven turn did not run on the in-cluster model"
    kubectl logs -n "$NAMESPACE" deploy/toolset-ctrl 2>/dev/null | grep -i 'prompt Job' | tail -5 >&2 || true
    return 1
  fi
}

# Adding a row admits an identity and removing it revokes,
# both without a pod restart. The relay logs each delivery it applies with
# the resulting table size (rows=N), so a new delivery line whose rows count
# moved is direct evidence the live table changed. Line counts scope the
# wait to deliveries after each patch; the startup delivery can't match.
step_6_grant_row_hot_reload() {
  step "Grant row hot reload"
  local before after count0 rows0 count1

  grants_deliveries() {
    kubectl logs deploy/relay-ctrl -n "$NAMESPACE" 2>/dev/null \
      | grep -c '"message":"grants delivery applied"'
  }
  grants_last_rows() {
    kubectl logs deploy/relay-ctrl -n "$NAMESPACE" 2>/dev/null \
      | grep '"message":"grants delivery applied"' | tail -1 \
      | grep -o '"rows":[0-9]*' | cut -d: -f2
  }

  before="$(kubectl get pod -n "$NAMESPACE" -l app.kubernetes.io/component=relay-ctrl \
    -o jsonpath='{.items[0].metadata.name}')"
  count0="$(grants_deliveries)"
  rows0="$(grants_last_rows)"
  if [ -z "$rows0" ]; then
    warn "no grants delivery in relay logs before the probe — watcher never synced"
    return 1
  fi

  kubectl patch configmap relay-grants -n "$NAMESPACE" --type=merge \
    -p '{"data":{"e2e-probe-row":"channel: app\nidentity: e2e-probe-identity\nworkspace: hello-world\n"}}' \
    >/dev/null
  if wait_for "relay applies the added row (rows $rows0 -> $((rows0 + 1)))" 60 \
       "[ \"\$(grants_deliveries)\" -gt $count0 ] && [ \"\$(grants_last_rows)\" -eq $((rows0 + 1)) ]"; then
    ok "Added row applied without a restart (rows=$((rows0 + 1)))"
  else
    warn "relay never applied the added grant row"
    return 1
  fi

  count1="$(grants_deliveries)"
  kubectl patch configmap relay-grants -n "$NAMESPACE" --type=json \
    -p '[{"op":"remove","path":"/data/e2e-probe-row"}]' >/dev/null
  if wait_for "relay applies the removal (rows back to $rows0)" 60 \
       "[ \"\$(grants_deliveries)\" -gt $count1 ] && [ \"\$(grants_last_rows)\" -eq $rows0 ]"; then
    ok "Removed row left the live table (rows=$rows0)"
  else
    warn "relay never applied the row removal — revocation did not land"
    return 1
  fi

  after="$(kubectl get pod -n "$NAMESPACE" -l app.kubernetes.io/component=relay-ctrl \
    -o jsonpath='{.items[0].metadata.name}')"
  if [ "$before" != "$after" ]; then
    warn "relay pod restarted during the hot-reload probe ($before -> $after)"
    return 1
  fi
  ok "Hot reload completed with no relay restart"
}

# ---- step 7: syco upgrade via the CLI ----
# Exercises the operator upgrade path — the earlier steps deploy with raw helm,
# so this is the only coverage that `syco upgrade` actually delivers the binary's
# charts. Wiping the config-root charts first is the regression guard: an upgrade
# that skips the chart re-extract has nothing to apply and fails here.
step_7_upgrade_cli() {
  step "Step 7: syco upgrade (CLI chart delivery)"

  cargo build --release -p syco >/dev/null
  local syco="$REPO_ROOT/target/release/syco"

  rm -rf "${HOME}/.config/sycophant/charts"

  if "$syco" upgrade; then
    ok "syco upgrade succeeded (re-extracted charts + applied cluster + tenant)"
  else
    warn "syco upgrade FAILED — CLI did not deliver its embedded charts"
    return 1
  fi

  if [ -f "${HOME}/.config/sycophant/charts/sycophant-cluster/Chart.yaml" ]; then
    ok "config-root charts re-extracted from the binary"
  else
    warn "syco upgrade left the config-root charts missing"
    return 1
  fi
}

main() {
  if [ "${SKIP_PREFLIGHT:-}" = "1" ]; then
    warn "SKIP_PREFLIGHT=1 — skipping prerequisite checks"
  else
    "$REPO_ROOT/scripts/preflight.sh"
  fi
  if [ "${SKIP_BOOTSTRAP:-}" = "1" ]; then
    warn "SKIP_BOOTSTRAP=1 — reusing existing cluster"
  else
    step_0_bootstrap
  fi
  if [ "${SKIP_BUILD:-}" = "1" ]; then
    warn "SKIP_BUILD=1 — reusing existing images"
  else
    step_1_build
  fi
  step_2_configure
  step_3_deploy
  step_4_verify
  step_5_flutter
  step_6_security
  step_7_upgrade_cli
  printf '\n\033[1;32m==> e2e complete\033[0m\n'
}

main "$@"

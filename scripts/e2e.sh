#!/usr/bin/env bash
# Single-command end-to-end test for sycophant.
#
# Bootstraps a clean k3d cluster, builds + loads all images, deploys the
# Helm charts (Layer 1 — no headscale, no tsnet bridge), launches the
# Pixel Android emulator with the Flutter client, and runs the Step 6
# security assertions. Pauses for the operator at the Flutter UI step.
#
# Layer 3 (phone-on-cellular via headscale + tsnetBridge) is NOT in
# scope; the emulator path validates the same auth wire format with
# zero router setup.
#
# Required env:
#   MISTRAL_API_KEY, ANTHROPIC_API_KEY
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
K3D_NODE="k3d-${CLUSTER_NAME}-server-0"
CLIENT_NAME="${CLIENT_NAME:-calebs-pixel}"
EMULATOR_NAME="${EMULATOR_NAME:-Pixel_9_API_36}"

: "${MISTRAL_API_KEY:?must be set}"
: "${ANTHROPIC_API_KEY:?must be set}"

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
cleanup() {
  local pid
  for pid in "${CLEANUP_PIDS[@]:-}"; do
    [ -n "$pid" ] && kill "$pid" 2>/dev/null || true
  done
}
trap cleanup EXIT

# ---- step 0: bootstrap ----
step_0_bootstrap() {
  step "Step 0: Bootstrap cluster"

  k3d cluster delete "$CLUSTER_NAME" 2>/dev/null || true

  k3d cluster create "$CLUSTER_NAME" \
    --k3s-arg "--flannel-backend=none@server:*" \
    --k3s-arg "--disable-network-policy@server:*" \
    --k3s-arg "--disable=traefik@server:*" \
    --k3s-arg "--disable=servicelb@server:*" \
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
  smoke_gvisor
  install_agent_sandbox
  install_kyverno
}

install_gvisor() {
  step "Step 0.3: gVisor (runsc) install"
  local url="https://storage.googleapis.com/gvisor/releases/release/latest/${ARCH}"
  local tmp
  tmp="$(mktemp -d)"
  ( cd "$tmp"
    curl -sSfL -o runsc                            "$url/runsc"
    curl -sSfL -o runsc.sha512                     "$url/runsc.sha512"
    curl -sSfL -o containerd-shim-runsc-v1         "$url/containerd-shim-runsc-v1"
    curl -sSfL -o containerd-shim-runsc-v1.sha512  "$url/containerd-shim-runsc-v1.sha512"
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

  kubectl apply -f - <<EOF
apiVersion: node.k8s.io/v1
kind: RuntimeClass
metadata:
  name: gvisor
handler: runsc
EOF
  ok "gVisor + RuntimeClass installed"
}

install_cilium() {
  step "Step 0.4: Cilium"
  local api_host
  api_host="$(docker inspect "$K3D_NODE" -f '{{ range $k, $v := .NetworkSettings.Networks }}{{ $v.IPAddress }}{{ end }}')"
  helm repo add cilium https://helm.cilium.io/ >/dev/null
  helm repo update >/dev/null
  helm upgrade --install cilium cilium/cilium --version 1.19.3 \
    --namespace kube-system \
    --set k8sServiceHost="$api_host" \
    --set k8sServicePort=6443 \
    --set kubeProxyReplacement=false \
    --set cni.exclusive=false >/dev/null
  kubectl rollout status -n kube-system ds/cilium --timeout=180s >/dev/null
  ok "Cilium ready"
}

smoke_gvisor() {
  step "Step 0.5: gVisor smoke test"
  kubectl delete pod gvisor-smoke --ignore-not-found --grace-period=0 --force >/dev/null 2>&1 || true
  kubectl run gvisor-smoke --restart=Never \
    --overrides='{"spec":{"runtimeClassName":"gvisor"}}' \
    --image=busybox:stable --command -- dmesg >/dev/null
  kubectl wait pod/gvisor-smoke --for=jsonpath='{.status.phase}'=Succeeded --timeout=60s >/dev/null
  if kubectl logs pod/gvisor-smoke | grep -q 'Starting gVisor'; then
    ok "gVisor sandbox boots"
  else
    warn "gVisor first dmesg line did NOT match 'Starting gVisor'"
    kubectl logs pod/gvisor-smoke | head -3
    return 1
  fi
  kubectl delete pod gvisor-smoke --ignore-not-found --grace-period=0 --force >/dev/null 2>&1 || true
}

install_agent_sandbox() {
  step "Step 0.6: Agent Sandbox v0.4.5"
  kubectl apply -f https://github.com/kubernetes-sigs/agent-sandbox/releases/download/v0.4.5/manifest.yaml   >/dev/null
  kubectl apply -f https://github.com/kubernetes-sigs/agent-sandbox/releases/download/v0.4.5/extensions.yaml >/dev/null
  ok "Agent Sandbox applied"
}

install_kyverno() {
  step "Step 0.7: Kyverno"
  helm repo add kyverno https://kyverno.github.io/kyverno/ >/dev/null
  helm repo update >/dev/null
  helm upgrade --install kyverno kyverno/kyverno -n kyverno --create-namespace --wait >/dev/null
  ok "Kyverno ready"
}

# ---- step 1: build images ----
step_1_build() {
  step "Step 1: Build images"
  cd "$REPO_ROOT"

  cargo build --release --target "$RUST_TARGET" \
    -p tightbeam-controller -p tightbeam-llm-job \
    -p airlock-controller -p airlock-runtime \
    -p transponder -p mainframe-runtime -p mainframe-controller

  local bin
  for bin in tightbeam-controller tightbeam-llm-job airlock-controller airlock-runtime mainframe-controller; do
    cp "target/$RUST_TARGET/release/$bin" "${bin}-linux-musl-${DOCKER_ARCH}"
    docker build -q -f build/Dockerfile \
      --build-arg "BINARY=$bin" --build-arg "TARGETARCH=$DOCKER_ARCH" \
      -t "${bin}:local" . >/dev/null
    rm "${bin}-linux-musl-${DOCKER_ARCH}"
  done

  cp "target/$RUST_TARGET/release/transponder" "transponder-linux-musl-${DOCKER_ARCH}"
  docker build -q -f build/Dockerfile \
    --build-arg BINARY=transponder --build-arg "TARGETARCH=$DOCKER_ARCH" \
    -t sycophant-transponder:local . >/dev/null
  rm "transponder-linux-musl-${DOCKER_ARCH}"

  cp "target/$RUST_TARGET/release/mainframe-runtime" /tmp/mainframe-runtime
  cat >/tmp/Dockerfile.mainframe-runtime <<'EOF'
FROM alpine:3.21
RUN apk add --no-cache git
COPY --chmod=755 mainframe-runtime /usr/local/bin/mainframe-runtime
ENTRYPOINT ["mainframe-runtime"]
EOF
  docker build -q -f /tmp/Dockerfile.mainframe-runtime -t sycophant-mainframe-runtime:local /tmp/ >/dev/null
  rm /tmp/mainframe-runtime /tmp/Dockerfile.mainframe-runtime

  cp "target/$RUST_TARGET/release/airlock-runtime" "images/git/airlock-runtime-linux-${DOCKER_ARCH}"
  docker build -q --build-arg "TARGETARCH=$DOCKER_ARCH" -f images/git/Dockerfile images/git/ -t airlock-git:local >/dev/null
  rm "images/git/airlock-runtime-linux-${DOCKER_ARCH}"

  cp "target/$RUST_TARGET/release/airlock-runtime" "examples/chambers/ssh-credentials/airlock-runtime-linux-${DOCKER_ARCH}"
  docker build -q --build-arg "TARGETARCH=$DOCKER_ARCH" examples/chambers/ssh-credentials/ -t airlock-ssh-credentials:local >/dev/null
  rm "examples/chambers/ssh-credentials/airlock-runtime-linux-${DOCKER_ARCH}"

  step "Loading images into k3d + pushing chambers to registry"
  local img
  for img in tightbeam-controller:local tightbeam-llm-job:local \
             airlock-controller:local mainframe-controller:local \
             sycophant-transponder:local sycophant-mainframe-runtime:local; do
    k3d image import "$img" --cluster "$CLUSTER_NAME" >/dev/null
  done
  for img in airlock-git airlock-ssh-credentials; do
    docker tag "${img}:local" "localhost:5555/${img}:latest"
    docker push -q "localhost:5555/${img}:latest" >/dev/null
  done
  ok "Images built + loaded"
}

# ---- step 2: configure ----
step_2_configure() {
  step "Step 2: Configure namespace, RBAC, kernels, secrets"

  kubectl create namespace "$NAMESPACE" --dry-run=client -o yaml | kubectl apply -f - >/dev/null

  # Kyverno generator workaround — the tenant-rolebinding-generator only
  # fires for namespaces named tenant-* created by the deployer SA; e2e
  # uses a static name so we mint the TokenReview bindings ourselves.
  kubectl apply -f - <<EOF >/dev/null
apiVersion: rbac.authorization.k8s.io/v1
kind: ClusterRoleBinding
metadata: { name: ${NAMESPACE}-airlock-tokenreview }
roleRef:   { apiGroup: rbac.authorization.k8s.io, kind: ClusterRole, name: sycophant-airlock-tokenreview }
subjects: [ { kind: ServiceAccount, name: airlock-ctrl, namespace: ${NAMESPACE} } ]
---
apiVersion: rbac.authorization.k8s.io/v1
kind: ClusterRoleBinding
metadata: { name: ${NAMESPACE}-tightbeam-tokenreview }
roleRef:   { apiGroup: rbac.authorization.k8s.io, kind: ClusterRole, name: sycophant-tightbeam-tokenreview }
subjects: [ { kind: ServiceAccount, name: tightbeam-ctrl, namespace: ${NAMESPACE} } ]
EOF

  mkdir -p "$HOME/sycophant/tmp/hello-world-data"
  cp "$REPO_ROOT/examples/mainframe/simple/AGENTS.md" "$HOME/sycophant/tmp/hello-world-data/AGENTS.md"

  mkdir -p "$HOME/sycophant/tmp/multi-agent-data"
  cp -R "$REPO_ROOT/examples/mainframe/orchestrator/." "$HOME/sycophant/tmp/multi-agent-data/"

  kubectl create secret generic sycophant-llm-mistral   -n "$NAMESPACE" \
    --from-literal=api-key="$MISTRAL_API_KEY"   --dry-run=client -o yaml | kubectl apply -f - >/dev/null
  kubectl create secret generic sycophant-llm-anthropic -n "$NAMESPACE" \
    --from-literal=api-key="$ANTHROPIC_API_KEY" --dry-run=client -o yaml | kubectl apply -f - >/dev/null

  kubectl apply -f "$REPO_ROOT/examples/chambers/ssh-credentials/fixtures/" -n "$NAMESPACE" >/dev/null
  ok "Namespace, RBAC, kernels, secrets, chamber fixtures applied"
}

# ---- step 3: deploy ----
step_3_deploy() {
  step "Step 3: Deploy charts"

  helm upgrade --install sycophant "$REPO_ROOT/charts/sycophant-cluster/" \
    -n infra --create-namespace --wait >/dev/null
  ok "Cluster chart installed"

  kubectl label namespace "$NAMESPACE" app.kubernetes.io/part-of=sycophant-tenant --overwrite >/dev/null

  helm upgrade --install "$NAMESPACE" "$REPO_ROOT/charts/sycophant-tenant/" \
    -n "$NAMESPACE" \
    -f "$REPO_ROOT/docs/e2e/values.yaml" \
    --set "clients.${CLIENT_NAME}.workspaces={hello-world}" \
    --wait --timeout=5m \
    >/dev/null
  ok "Tenant chart installed (Layer 1; client: ${CLIENT_NAME})"

  # Chamber images aren't part of the chart's checksum tracking, so a
  # rebuild + push of a chamber doesn't trigger airlock-ctrl to re-pull
  # and re-discover tool labels. Force it to re-pick up by restarting.
  kubectl rollout restart -n "$NAMESPACE" deploy/airlock-ctrl >/dev/null
  kubectl rollout status -n "$NAMESPACE" deploy/airlock-ctrl --timeout=120s >/dev/null
  ok "airlock-ctrl re-rolled (fresh chamber discovery)"
}

# ---- step 4: verify chart ----
step_4_verify() {
  step "Step 4: Verify chart"

  # Workspace pods are created by the mainframe-controller off a Sandbox CR
  # — they don't exist the instant `helm --wait` returns. Poll for the
  # pod object first, then wait for Ready.
  wait_for "hello-world pod object" 120 \
    "kubectl get pod hello-world -n '$NAMESPACE' >/dev/null 2>&1"
  kubectl wait -n "$NAMESPACE" --for=condition=Ready --timeout=180s \
    pod/hello-world >/dev/null
  ok "hello-world workspace Ready"

  if kubectl get pod multi-agent -n "$NAMESPACE" >/dev/null 2>&1 && \
     kubectl wait -n "$NAMESPACE" --for=condition=Ready --timeout=10s \
       pod/multi-agent >/dev/null 2>&1; then
    ok "multi-agent workspace Ready"
  else
    warn "multi-agent not Ready (Docker Desktop memory constraint) — Flutter test only uses hello-world, continuing"
  fi
}

# ---- step 5: flutter ----
step_5_flutter() {
  step "Step 5: Flutter emulator + chat"

  kubectl port-forward -n "$NAMESPACE" deploy/tightbeam-ctrl 9091:9091 --address 0.0.0.0 \
    >/dev/null 2>&1 &
  CLEANUP_PIDS+=($!)

  local code=""
  if kubectl get tbcl "$CLIENT_NAME" -n "$NAMESPACE" \
       -o jsonpath='{.status.publicKey}' 2>/dev/null | grep -q .; then
    ok "Client ${CLIENT_NAME} already enrolled (status.publicKey set) — reusing"
  else
    step "Waiting for tightbeam-controller to mint enrollment code"
    wait_for "Client CR status.enrollmentCode" 60 \
      "kubectl get tbcl '$CLIENT_NAME' -n '$NAMESPACE' -o jsonpath='{.status.enrollmentCode}' 2>/dev/null | grep -q ."
    code="$(kubectl get tbcl "$CLIENT_NAME" -n "$NAMESPACE" -o jsonpath='{.status.enrollmentCode}')"
    ok "Enrollment code minted"
  fi

  step "Launching ${EMULATOR_NAME}"
  # `adb devices` (one line per attached device, state in column 2) is fast
  # + deterministic. `flutter devices` exits non-zero on the iPad wireless
  # scan, which trips `set -o pipefail` and never breaks an `until` loop.
  if ! adb devices 2>/dev/null | awk 'NR>1 && $2=="device" {found=1} END{exit !found}'; then
    flutter emulators --launch "$EMULATOR_NAME" >/dev/null 2>&1 || \
      { warn "flutter emulators --launch ${EMULATOR_NAME} failed (does the AVD exist?)"; return 1; }
  fi
  wait_for "Android device online via adb" 180 \
    "adb devices 2>/dev/null | awk 'NR>1 && \$2==\"device\" {f=1} END{exit !f}'"
  ok "Emulator online"

  step "Installing Flutter app on emulator"
  ( cd "$REPO_ROOT/client" && flutter run -d emulator-5554 ) >/tmp/sycophant-flutter-run.log 2>&1 &
  CLEANUP_PIDS+=($!)
  wait_for "Flutter app installed on emulator" 300 \
    "grep -q 'Flutter run key commands\\|Installing build' /tmp/sycophant-flutter-run.log 2>/dev/null"
  ok "Flutter app installed; emulator ready for enrollment"

  if [ -n "$code" ]; then
    printf '\n\033[1;35m========== Paste these into the app ==========\033[0m\n'
    printf '  Server:           10.0.2.2:9091\n'
    printf '  Workspace:        hello-world\n'
    printf '  Enrollment code:  %s\n' "$code"
    printf '\033[1;35m===============================================\033[0m\n'
  else
    printf '\n\033[1;35m========== App already enrolled ==========\033[0m\n'
    printf '  Just send the chat message below.\n'
    printf '\033[1;35m==========================================\033[0m\n'
  fi

  pause "Tap Enroll, then send EXACTLY this message:
     Use the test-cmd tool.
   The LLM must call test-cmd (Step 6 asserts on airlock exec + scrubber)."
}

# ---- step 6: security assertions ----
step_6_security() {
  step "Step 6: Security assertions"

  local first_line
  first_line="$(kubectl exec -n "$NAMESPACE" hello-world -c mainframe-runtime -- dmesg 2>/dev/null | head -1)"
  if echo "$first_line" | grep -q 'Starting gVisor'; then
    ok "gVisor kernel isolation"
  else
    warn "gVisor first dmesg line was: $first_line"
    return 1
  fi

  local leaked
  leaked="$(kubectl logs -n "$NAMESPACE" hello-world -c transponder | grep -c 'FAKE-ED25519-PRIVATE-KEY' || true)"
  if [ "$leaked" -eq 0 ]; then
    ok "Secret scrubbing (0 raw demo-key matches in transponder log)"
  else
    warn "transponder log contains $leaked unscrubbed key bytes"
    return 1
  fi

  if kubectl logs -n "$NAMESPACE" deployment/airlock-ctrl | grep -q 'received tool result.*exit_code=0'; then
    ok "Tool execution (airlock saw exit_code=0)"
  else
    warn "no exit_code=0 tool result in airlock-ctrl log"
    return 1
  fi

  if kubectl exec -n "$NAMESPACE" hello-world -c mainframe-runtime -- \
       wget -qO- --timeout=3 https://httpbin.org/ip >/dev/null 2>&1; then
    warn "workspace reached httpbin.org — NetworkPolicy egress NOT enforced"
    return 1
  else
    ok "NetworkPolicy blocks workspace egress"
  fi

  if kubectl exec -n "$NAMESPACE" hello-world -c mainframe-runtime -- \
       cat /run/secrets/llm/api-key >/dev/null 2>&1; then
    warn "/run/secrets/llm/api-key exists inside workspace pod — credential leak"
    return 1
  else
    ok "Credential isolation (no LLM key in workspace pod)"
  fi

  if kubectl get serviceaccounts -n "$NAMESPACE" -l sycophant.io/type=workspace-sa -o name \
       | grep -q sa-hello-world; then
    ok "Workspace ServiceAccounts present"
  else
    warn "sa-hello-world ServiceAccount missing"
    return 1
  fi
}

main() {
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
  printf '\n\033[1;32m==> e2e complete\033[0m\n'
}

main "$@"

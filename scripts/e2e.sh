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
K3D_NODE="k3d-${CLUSTER_NAME}-server-0"
# Client choice. Prompt when unset + interactive (the "confirm during install"
# step); honor the env var otherwise so CI/agents run non-interactively.
# none = backend-only: skip the local Flutter client and connect one remotely.
if [ -z "${FLUTTER_TARGET:-}" ]; then
  if [ -t 0 ]; then
    printf 'Install a Flutter client? [macos/android/none] (none = backend-only): ' >&2
    read -r FLUTTER_TARGET
    FLUTTER_TARGET="${FLUTTER_TARGET:-none}"
  else
    FLUTTER_TARGET="macos"
  fi
fi
case "$FLUTTER_TARGET" in
  macos)   CLIENT_NAME_DEFAULT="caleb-macbook" ;;
  android) CLIENT_NAME_DEFAULT="calebs-pixel" ;;
  none)    CLIENT_NAME_DEFAULT="remote-client" ;;
  *) printf 'unknown FLUTTER_TARGET: %s (expected macos|android|none)\n' "$FLUTTER_TARGET" >&2; exit 1 ;;
esac
CLIENT_NAME="${CLIENT_NAME:-$CLIENT_NAME_DEFAULT}"
EMULATOR_NAME="${EMULATOR_NAME:-Pixel_9_API_36}"

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

# Airlock-ctrl resolves chamber image refs from inside the cluster to read
# tool labels off the image manifest. CoreDNS doesn't know about the
# `sycophant-registry` container (it's on the k3d Docker network, not in
# Kubernetes Services), so we patch its NodeHosts to add the entry. Image
# refs in Chamber CRs use `sycophant-registry:5000/...` (in-cluster name +
# port); without this patch, the hostname is NXDOMAIN and tool discovery
# fails silently — Step 6 then can't find any chamber tool execution.
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
  kubectl rollout status deploy/coredns -n kube-system --timeout=60s >/dev/null
  local dns_deadline=$((SECONDS + 60))
  while (( SECONDS < dns_deadline )); do
    if kubectl run "dns-probe-$$" --rm -i --restart=Never --image=busybox:1.36 \
         --quiet --timeout=10s -- nslookup sycophant-registry >/dev/null 2>&1; then
      ok "CoreDNS resolves sycophant-registry -> ${registry_ip}"
      return 0
    fi
    sleep 2
  done
  warn "sycophant-registry did not resolve from a workload pod within 60s (continuing; airlock-ctrl will retry)"
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

  helm upgrade --install sycophant-gvisor "$REPO_ROOT/charts/sycophant-gvisor" --wait >/dev/null
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
    --set "ipam.operator.clusterPoolIPv4PodCIDRList={10.42.0.0/16}" >/dev/null
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
    -p hangar-controller -p hangar-llm-job \
    -p airlock-controller -p airlock-runtime \
    -p transponder -p mainframe-controller -p tightbeam-controller

  local bin
  for bin in hangar-controller hangar-llm-job airlock-controller airlock-runtime mainframe-controller tightbeam-controller; do
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

  cp "target/$RUST_TARGET/release/airlock-runtime" "images/airlock-chamber/airlock-runtime-linux-${DOCKER_ARCH}"
  docker build -q --build-arg "TARGETARCH=$DOCKER_ARCH" -f images/airlock-chamber/Dockerfile images/airlock-chamber/ -t airlock-chamber:local >/dev/null
  rm "images/airlock-chamber/airlock-runtime-linux-${DOCKER_ARCH}"

  cp "target/$RUST_TARGET/release/airlock-runtime" "images/git/airlock-runtime-linux-${DOCKER_ARCH}"
  docker build -q --build-arg "TARGETARCH=$DOCKER_ARCH" -f images/git/Dockerfile images/git/ -t airlock-git:local >/dev/null
  rm "images/git/airlock-runtime-linux-${DOCKER_ARCH}"

  cp "target/$RUST_TARGET/release/airlock-runtime" "examples/chambers/ssh-credentials/airlock-runtime-linux-${DOCKER_ARCH}"
  docker build -q --build-arg "TARGETARCH=$DOCKER_ARCH" examples/chambers/ssh-credentials/ -t airlock-ssh-credentials:local >/dev/null
  rm "examples/chambers/ssh-credentials/airlock-runtime-linux-${DOCKER_ARCH}"

  docker build -q --build-arg "TARGETARCH=$DOCKER_ARCH" images/kubectl/ -t sycophant-kubectl:local >/dev/null

  step "Loading images into k3d + pushing chambers to registry"
  local img
  for img in hangar-controller:local hangar-llm-job:local \
             airlock-controller:local mainframe-controller:local \
             sycophant-transponder:local tightbeam-controller:local \
             sycophant-kubectl:local; do
    k3d image import "$img" --cluster "$CLUSTER_NAME" >/dev/null
  done
  # Chamber images go through the local registry (sycophant-registry:5000
  # in-cluster) so airlock-controller can fetch their OCI manifests for
  # tool discovery. The stdlib chamber rides the same path.
  for img in airlock-chamber airlock-git airlock-ssh-credentials; do
    docker tag "${img}:local" "localhost:5555/${img}:latest"
    docker push -q "localhost:5555/${img}:latest" >/dev/null
  done
  ok "Images built + loaded"
}

# ---- step 2: configure ----
step_2_configure() {
  step "Step 2: Configure namespace, RBAC, kernels, secrets"

  kubectl create namespace "$NAMESPACE" --dry-run=client -o yaml | kubectl apply -f - >/dev/null

  # TokenReview CRBs + the pod VAP binding are minted by Kyverno's
  # tenant-rolebinding-generator once the ns carries part-of=sycophant-tenant
  # (labelled in step_3, after the cluster chart installs the generator).

  # Kernel content lives at the convention path <hostPathBase>/<ns>/<workspace>;
  # the tenant install below sets hostPathBase to $HOME/sycophant/tmp (bind-
  # mounted into the node), so this dir surfaces at /etc/kernels/$NAMESPACE/hello-world.
  mkdir -p "$HOME/sycophant/tmp/$NAMESPACE/hello-world"
  cp "$REPO_ROOT/examples/mainframe/simple/AGENTS.md" "$HOME/sycophant/tmp/$NAMESPACE/hello-world/AGENTS.md"

  kubectl create secret generic sycophant-llm-openrouter -n "$NAMESPACE" \
    --from-literal=api-key="$OPENROUTER_API_KEY" --dry-run=client -o yaml | kubectl apply -f - >/dev/null

  kubectl apply -f "$REPO_ROOT/examples/chambers/ssh-credentials/fixtures/" -n "$NAMESPACE" >/dev/null
  ok "Namespace, RBAC, kernels, secrets, chamber fixtures applied"
}

# ---- step 3: deploy ----
step_3_deploy() {
  step "Step 3: Deploy charts"

  # Create the PSA-restricted release namespace before helm installs into it
  # (helm --create-namespace would land it bare). Same manifest `syco setup` applies.
  kubectl apply -f "$REPO_ROOT/charts/sycophant-cluster/system-ns.yaml" >/dev/null
  helm upgrade --install sycophant "$REPO_ROOT/charts/sycophant-cluster/" \
    -n sycophant-system --wait >/dev/null
  ok "Cluster chart installed"

  # Labelling the ns triggers the (label-matched) tenant-rolebinding-generator,
  # which mints the per-tenant TokenReview CRBs + the pod VAP binding. Kyverno
  # generate is async, so wait for the wiring before the controllers need it.
  kubectl label namespace "$NAMESPACE" app.kubernetes.io/part-of=sycophant-tenant --overwrite >/dev/null

  local crb
  for crb in airlock hangar mainframe tightbeam; do
    wait_for "${NAMESPACE}-${crb}-tokenreview CRB" 120 \
      "kubectl get clusterrolebinding ${NAMESPACE}-${crb}-tokenreview >/dev/null 2>&1"
  done
  wait_for "${NAMESPACE}-sycophant-pod-binding VAP binding" 120 \
    "kubectl get validatingadmissionpolicybinding ${NAMESPACE}-sycophant-pod-binding >/dev/null 2>&1"
  ok "Kyverno minted TokenReview CRBs + pod VAP binding (label-triggered)"

  # The pod VAP is now bound to this ns — assert it actually enforces. This pod
  # is PSA-restricted-compliant but sets automountServiceAccountToken: true,
  # which only the VAP forbids (PSA does not check it), so a denial citing the
  # policy proves the binding is live — it never existed in e2e before.
  local vap_probe_err
  vap_probe_err="$(kubectl apply -n "$NAMESPACE" -f - 2>&1 <<'POD' || true
apiVersion: v1
kind: Pod
metadata:
  name: vap-probe
  labels:
    app.kubernetes.io/part-of: sycophant
    app.kubernetes.io/component: hangar-ctrl
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
  else
    warn "pod VAP did NOT deny the probe (binding not enforcing): ${vap_probe_err}"
    exit 1
  fi

  # Resolve local-registry digests for chamber images. The host rewrite
  # (localhost:5555 → sycophant-registry:5000) swaps the docker-push-facing
  # host for the in-cluster name resolved via the CoreDNS NodeHosts entry
  # added in patch_coredns_for_registry; the digest is identical either way.
  local git_ref ssh_ref stdlib_ref
  git_ref="$(docker inspect --format '{{range .RepoDigests}}{{println .}}{{end}}' localhost:5555/airlock-git:latest | grep '^localhost:5555/')"
  ssh_ref="$(docker inspect --format '{{range .RepoDigests}}{{println .}}{{end}}' localhost:5555/airlock-ssh-credentials:latest | grep '^localhost:5555/')"
  stdlib_ref="$(docker inspect --format '{{range .RepoDigests}}{{println .}}{{end}}' localhost:5555/airlock-chamber:latest | grep '^localhost:5555/')"
  git_ref="${git_ref/localhost:5555/sycophant-registry:5000}"
  ssh_ref="${ssh_ref/localhost:5555/sycophant-registry:5000}"
  stdlib_ref="${stdlib_ref/localhost:5555/sycophant-registry:5000}"

  # The chart does not read Kernel CRs at render time; `syco tenant up` reads
  # this CR and passes the workspace's kernel kind (+ path) as a helm value that
  # renders the per-workspace PV — no render-time lookup.
  # HostPath content is delivered from the convention path set in step 2.
  kubectl apply -n "$NAMESPACE" -f - >/dev/null <<EOF
apiVersion: sycophant.md/v1
kind: Kernel
metadata:
  name: hello-world
  namespace: ${NAMESPACE}
  labels:
    app.kubernetes.io/part-of: sycophant
    sycophant.md/type: kernel
spec:
  kind: HostPath
EOF
  ok "Kernel CR (hello-world, HostPath) applied"

  # Readiness is gated by the install-wait post-install hook (helm waits for
  # hooks regardless of --wait), so native --wait is omitted here.
  # hostPathBase points at the bind-mounted node dir; content lives at
  # <base>/<ns>/<workspace> and surfaces at /etc/kernels/<namespace>/<workspace>.
  helm upgrade --install "$NAMESPACE" "$REPO_ROOT/charts/sycophant-tenant/" \
    -n "$NAMESPACE" \
    -f "$REPO_ROOT/docs/e2e/values.yaml" \
    --set-string "mainframe.kernels.hostPathBase=${HOME}/sycophant/tmp" \
    --timeout=5m \
    >/dev/null
  ok "Tenant chart installed (Layer 1; client: ${CLIENT_NAME})"

  # Enrollment is content, not chart config (applied operator-side, like providers/models).
  kubectl apply -n "$NAMESPACE" -f - >/dev/null <<EOF
apiVersion: sycophant.md/v1
kind: Enrollment
metadata:
  name: ${CLIENT_NAME}
  namespace: ${NAMESPACE}
  labels:
    app.kubernetes.io/part-of: sycophant
    sycophant.md/type: enrollment
spec:
  workspaces:
    - hello-world
EOF
  ok "Enrollment ${CLIENT_NAME} applied (content tier)"

  # OpenRouter is the sole provider; the default model is a cheap DeepSeek
  # model (deepseek/deepseek-v4-flash) for low-cost e2e runs. Providers/models
  # are content, applied operator-side like clients/chambers.
  kubectl apply -n "$NAMESPACE" \
    -f "$REPO_ROOT/examples/providers/openrouter.yaml" \
    -f "$REPO_ROOT/examples/models/default.yaml" >/dev/null
  ok "Provider (OpenRouter) + default model (deepseek-v4-flash) applied"

  # llm-job egress union CNP (authored externally by `syco provider`/`syco model
  # set`; hand-applied here to match that path — controllers no longer author
  # CNPs). Composes on the chart's llm-job-baseline floor; the LLM turn's llm-job
  # needs it to reach the provider API.
  kubectl apply -n "$NAMESPACE" -f - >/dev/null <<EOF
apiVersion: cilium.io/v2
kind: CiliumNetworkPolicy
metadata:
  name: llm-job-egress
  namespace: ${NAMESPACE}
  labels:
    app.kubernetes.io/part-of: sycophant
spec:
  endpointSelector:
    matchLabels:
      app.kubernetes.io/component: llm-job
  egress:
    - toEndpoints:
        - matchLabels:
            io.kubernetes.pod.namespace: kube-system
            k8s-app: kube-dns
      toPorts:
        - ports:
            - port: "53"
              protocol: UDP
            - port: "53"
              protocol: TCP
          rules:
            dns:
              - matchName: "hangar-ctrl.${NAMESPACE}.svc.cluster.local"
              - matchName: "openrouter.ai"
    - toEndpoints:
        - matchLabels:
            app.kubernetes.io/name: hangar-ctrl
      toPorts:
        - ports:
            - port: "9090"
              protocol: TCP
    - toFQDNs:
        - matchName: "openrouter.ai"
      toPorts:
        - ports:
            - port: "443"
              protocol: TCP
EOF
  ok "llm-job egress union CNP applied (content tier)"

  # Chambers are content (applied operator-side, like providers/models). Each
  # chamber's per-chamber egress CNP is authored externally by `syco chamber set`
  # (hand-applied below for stdlib) and composes on the chart's airlock-job-baseline.
  kubectl apply -n "$NAMESPACE" \
    -f "$REPO_ROOT/examples/chambers/stdlib/chamber.yaml" \
    -f "$REPO_ROOT/examples/chambers/workspace-ro/chamber.yaml" \
    -f "$REPO_ROOT/examples/chambers/ssh-credentials/chamber.yaml" >/dev/null
  kubectl patch chamber stdlib -n "$NAMESPACE" --type=merge \
    -p "{\"spec\":{\"image\":\"${stdlib_ref}\"}}" >/dev/null
  kubectl patch chamber workspace-ro -n "$NAMESPACE" --type=merge \
    -p "{\"spec\":{\"image\":\"${git_ref}\"}}" >/dev/null
  kubectl patch chamber ssh-credentials -n "$NAMESPACE" --type=merge \
    -p "{\"spec\":{\"image\":\"${ssh_ref}\"}}" >/dev/null
  ok "Chambers applied + patched to local-registry digests"

  # Per-chamber egress CNP, authored externally by `syco chamber set` (hand-applied
  # here to match that path). stdlib needs no external egress, so it's the universal
  # floor (DNS->airlock-ctrl + airlock-ctrl:9090), composing on airlock-job-baseline.
  kubectl apply -n "$NAMESPACE" -f - >/dev/null <<EOF
apiVersion: cilium.io/v2
kind: CiliumNetworkPolicy
metadata:
  name: airlock-chamber-stdlib
  namespace: ${NAMESPACE}
  labels:
    app.kubernetes.io/part-of: sycophant
    sycophant.md/type: chamber
    sycophant.md/chamber: stdlib
spec:
  endpointSelector:
    matchLabels:
      sycophant.md/chamber: stdlib
  egress:
    - toEndpoints:
        - matchLabels:
            io.kubernetes.pod.namespace: kube-system
            k8s-app: kube-dns
      toPorts:
        - ports:
            - port: "53"
              protocol: UDP
            - port: "53"
              protocol: TCP
          rules:
            dns:
              - matchName: "airlock-ctrl.${NAMESPACE}.svc.cluster.local"
    - toEndpoints:
        - matchLabels:
            app.kubernetes.io/component: airlock-ctrl
      toPorts:
        - ports:
            - port: "9090"
              protocol: TCP
EOF
  ok "stdlib per-chamber egress CNP applied (content tier)"

  # The two fail-closed baselines are chart-rendered (present from install); the
  # two content CNPs are operator-applied. All four must exist — the structural
  # proof that egress authoring moved OUT of the tenant.
  for cnp in airlock-job-baseline llm-job-baseline airlock-chamber-stdlib llm-job-egress; do
    if kubectl get ciliumnetworkpolicy "$cnp" -n "$NAMESPACE" >/dev/null 2>&1; then
      ok "CNP present: $cnp"
    else
      warn "expected CNP missing: $cnp"
      exit 1
    fi
  done
}

# ---- step 4: verify chart ----
step_4_verify() {
  step "Step 4: Verify chart"

  # Per-workspace transponder Deployment rendered by the chart. Wait on
  # the Deployment becoming Available rather than a specific pod name,
  # since the pod name now carries a ReplicaSet suffix.
  kubectl wait -n "$NAMESPACE" --for=condition=Available --timeout=180s \
    deployment/hello-world >/dev/null
  ok "hello-world workspace Ready"

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

  # Stdlib chamber Chamber CR must exist by now (helm rendered it). Chamber
  # pods are airlock-spawned lazily on the first CallTool RPC, so zero pods
  # is the correct pre-tool-call state. Step 6 verifies the pod appears
  # after the first call and survives subsequent calls (keepalive=true).
  if ! kubectl get chamber stdlib -n "$NAMESPACE" >/dev/null 2>&1; then
    warn "Chamber CR 'stdlib' missing — helm render or airlock-controller failed"
    return 1
  fi
  ok "Chamber CR 'stdlib' present (keepalive=true, lazy-spawn)"
}

# ---- step 5: flutter ----
step_5_flutter_port_forward() {
  # Self-healing loop: kubectl port-forward binds to one pod's stream and
  # dies on pod replacement (rollout, eviction, crash). The loop reconnects
  # to the deployment's current pod automatically. Disable errexit inside
  # the subshell so a non-zero kubectl exit doesn't kill the loop. The trap
  # ensures the kubectl child dies with the subshell — without it the
  # kubectl process would be orphaned and survive script teardown.
  ( set +e
    kpid=""
    trap '[ -n "$kpid" ] && kill "$kpid" 2>/dev/null; exit' TERM INT EXIT
    while true; do
      kubectl port-forward -n "$NAMESPACE" deploy/tightbeam-ctrl 9091:9091 --address 0.0.0.0 \
        >/dev/null 2>&1 &
      kpid=$!
      wait "$kpid"
      sleep 2
    done ) &
  CLEANUP_PIDS+=($!)
}

step_5_flutter_enrollment_code() {
  if kubectl get enr "$CLIENT_NAME" -n "$NAMESPACE" \
       -o jsonpath='{.status.publicKey}' 2>/dev/null | grep -q .; then
    ok "Enrollment ${CLIENT_NAME} already redeemed (status.publicKey set) — reusing"
    printf ''
    return 0
  fi
  step "Waiting for tightbeam-controller to mint enrollment code"
  wait_for "Enrollment status.enrollmentCode" 60 \
    "kubectl get enr '$CLIENT_NAME' -n '$NAMESPACE' -o jsonpath='{.status.enrollmentCode}' 2>/dev/null | grep -q ."
  kubectl get enr "$CLIENT_NAME" -n "$NAMESPACE" -o jsonpath='{.status.enrollmentCode}'
  ok "Enrollment code minted" >&2
}

step_5_flutter_android() {
  local code="$1"
  step "Launching ${EMULATOR_NAME}"
  # `adb devices` (one line per attached device, state in column 2) is fast
  # + deterministic. `flutter devices` exits non-zero on the iPad wireless
  # scan, which trips `set -o pipefail` and never breaks an `until` loop.
  if ! adb devices 2>/dev/null | awk 'NR>1 && $2=="device" {found=1} END{exit !found}'; then
    flutter emulators --launch "$EMULATOR_NAME" >/dev/null 2>&1 || \
      { warn "flutter emulators --launch ${EMULATOR_NAME} failed (does the AVD exist?)"; return 1; }
  fi
  # ADB's `start-server` sometimes registers the emulator as `offline`
  # and stays stuck even after boot completes. If we still see `offline`
  # after 30s, restart the ADB server once to clear the stale state.
  local emu_deadline=$(( $(date +%s) + 240 ))
  local adb_kicked=0
  while true; do
    local state
    state=$(adb devices 2>/dev/null | awk 'NR>1 && /^emulator-/ {print $2; exit}')
    if [ "$state" = "device" ]; then
      ok "Emulator online"
      break
    fi
    if [ "$(date +%s)" -ge "$emu_deadline" ]; then
      warn "timeout after 240s waiting for emulator online; last state: ${state:-none}"
      return 1
    fi
    if [ "$state" = "offline" ] && [ "$adb_kicked" -eq 0 ]; then
      local elapsed_offline=$(( $(date +%s) - (emu_deadline - 240) ))
      if [ "$elapsed_offline" -ge 30 ]; then
        warn "emulator stuck offline for ${elapsed_offline}s; restarting adb server"
        adb kill-server >/dev/null 2>&1 || true
        sleep 2
        adb start-server >/dev/null 2>&1 || true
        adb_kicked=1
      fi
    fi
    sleep 2
  done

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
    printf '  Server:           127.0.0.1:9091\n'
    printf '  Workspace:        hello-world\n'
    printf '  Enrollment code:  %s\n' "$code"
    printf '\033[1;35m===============================================\033[0m\n'
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

  step_5_flutter_port_forward
  local code
  code="$(step_5_flutter_enrollment_code)"

  local addr="127.0.0.1:9091"
  if command -v tailscale >/dev/null 2>&1; then
    local ts_ip
    ts_ip="$(tailscale ip -4 2>/dev/null | head -1 || true)"
    [ -n "$ts_ip" ] && addr="${ts_ip}:9091"
  fi

  printf '\n  Backend is up. Connect a client from another machine:\n' >&2
  printf '    Server:          %s\n' "$addr" >&2
  printf '    Workspace:       hello-world\n' >&2
  printf '    Namespace:       %s\n' "$NAMESPACE" >&2
  printf '    Client:          %s\n' "$CLIENT_NAME" >&2
  printf '    Enrollment code: %s\n' "$code" >&2

  pause "From the other machine, point the app at ${addr}, enroll with the code
   above, then send EXACTLY this message:
     Use the test-cmd tool, then use the Bash tool to run \`dmesg | head -1\`.
   (Step 6 asserts on the airlock chamber tool + the stdlib chamber pod this
   triggers — same as the local-client path.)"
}

step_5_flutter() {
  if [ "$FLUTTER_TARGET" = "none" ]; then
    step_5_backend_only
    return
  fi

  step "Step 5: Flutter ${FLUTTER_TARGET} + chat"

  step_5_flutter_port_forward
  local code
  code="$(step_5_flutter_enrollment_code)"

  case "$FLUTTER_TARGET" in
    macos)   step_5_flutter_macos "$code" ;;
    android) step_5_flutter_android "$code" ;;
  esac

  pause "Tap Enroll, then send EXACTLY this message:
     Use the test-cmd tool, then use the Bash tool to run \`dmesg | head -1\`.
   The LLM must call BOTH test-cmd (tenant airlock chamber tool — Step 6
   asserts on airlock exec + scrubber) AND Bash (stdlib chamber tool —
   spawns the per-workspace stdlib chamber pod that Step 6 asserts on
   for gVisor + egress + credential isolation, then verifies the pod
   survives the next call via keepalive)."
}

# ---- step 6: security assertions ----
step_6_security() {
  step "Step 6: Security assertions"

  # Wait for the per-workspace stdlib chamber pod (lazy-spawned by
  # airlock-ctrl on the first stdlib Bash/ReadFile/WriteFile/ListDirectory
  # call from the agent). 90s buffer accounts for the known ARM64 gVisor
  # `epoll_pwait` slow path on first cold start — see vault
  # `sycophant-kernel-isolation-runtime`.
  local chamber_selector="app.kubernetes.io/component=airlock-job,sycophant.md/workspace=hello-world,sycophant.md/chamber=stdlib"
  local task_pod
  wait_for "stdlib chamber pod for hello-world" 90 \
    "kubectl get pod -n '$NAMESPACE' -l '$chamber_selector' -o name 2>/dev/null | grep -q ."
  task_pod="$(kubectl get pod -n "$NAMESPACE" \
                -l "$chamber_selector" \
                -o jsonpath='{.items[0].metadata.name}')"
  kubectl wait -n "$NAMESPACE" --for=condition=Ready --timeout=60s "pod/$task_pod" >/dev/null
  ok "stdlib chamber pod Ready ($task_pod)"

  local first_line
  first_line="$(kubectl exec -n "$NAMESPACE" "$task_pod" -- dmesg 2>/dev/null | head -1)"
  if echo "$first_line" | grep -q 'Starting gVisor'; then
    ok "gVisor kernel isolation"
  else
    warn "gVisor first dmesg line was: $first_line"
    return 1
  fi

  # Scan for real API-key prefixes in two sinks:
  #   1. transponder stdout (kubectl logs)
  #   2. conversation log files on the transponder's conversation-data PVC
  # Patterns match a prefix + length floor — `sk-ant-` + 50+ base64 chars
  # for Anthropic, `sk-` + 40+ for generic OpenAI-style. The length floor
  # avoids false positives on the bare strings "sk-" or "sk-ant-" appearing
  # in normal text.
  local key_regex='sk-ant-[A-Za-z0-9_-]{50,}|sk-[A-Za-z0-9_-]{40,}'

  local transponder_hits
  transponder_hits="$(kubectl logs -n "$NAMESPACE" deployment/hello-world -c transponder --tail=10000 2>/dev/null \
                        | grep -cE "$key_regex" || true)"

  # The conversation log is on the transponder's OWN RWO PVC
  # (<ws>-conversation-data at /var/lib/transponder/conversations). A separate
  # pod can't mount an RWO PVC, and the transponder image is FROM scratch (no
  # shell), so attach an ephemeral busybox to the transponder pod sharing its
  # PID namespace and read the dir via /proc/1/root. (Fallback if a hardened
  # node blocks /proc/1/root via ptrace_scope: scale the transponder to 0,
  # mount <ws>-conversation-data RO in a probe pod, grep, then scale back to 1.)
  local tb_pod scrub_c patch
  tb_pod="$(kubectl get pod -n "$NAMESPACE" \
    -l app.kubernetes.io/component=transponder,sycophant.md/workspace=hello-world \
    -o jsonpath='{.items[0].metadata.name}')"
  scrub_c="syco-scrub-$$"
  patch='{"spec":{"ephemeralContainers":[{"name":"'"$scrub_c"'","image":"busybox:1.36","command":["sleep","60"],"targetContainerName":"transponder","securityContext":{"runAsNonRoot":true,"runAsUser":1000,"readOnlyRootFilesystem":true,"allowPrivilegeEscalation":false,"capabilities":{"drop":["ALL"]},"seccompProfile":{"type":"RuntimeDefault"}}}]}}'
  kubectl patch pod "$tb_pod" -n "$NAMESPACE" \
    --subresource=ephemeralcontainers --type=strategic -p "$patch" >/dev/null
  local conv_hits=""
  for _ in $(seq 1 15); do
    conv_hits="$(kubectl exec -n "$NAMESPACE" "$tb_pod" -c "$scrub_c" -- \
      sh -c "grep -rcE '$key_regex' /proc/1/root/var/lib/transponder/conversations 2>/dev/null | grep -v ':0\$' | wc -l" 2>/dev/null)" && break
    sleep 2
  done
  conv_hits="${conv_hits//[[:space:]]/}"
  conv_hits="${conv_hits:-0}"

  if [ "$transponder_hits" -eq 0 ] && [ "$conv_hits" -eq 0 ]; then
    ok "Secret scrubbing (0 sk-ant-/sk- matches in transponder + conv log)"
  else
    warn "Unscrubbed key prefixes detected: transponder=$transponder_hits conv_log=$conv_hits"
    return 1
  fi

  # airlock-ctrl emits structured JSON via `tracing_subscriber::fmt().json()`,
  # so the field appears as `"exit_code":0`, not `exit_code=0`.
  if kubectl logs -n "$NAMESPACE" deployment/airlock-ctrl | grep -q '"message":"received tool result".*"exit_code":0'; then
    ok "Tool execution (airlock saw exit_code=0)"
  else
    warn "no exit_code=0 tool result in airlock-ctrl log"
    return 1
  fi

  if kubectl exec -n "$NAMESPACE" "$task_pod" -- \
       wget -qO- --timeout=3 https://httpbin.org/ip >/dev/null 2>&1; then
    warn "stdlib chamber pod reached httpbin.org — NetworkPolicy egress NOT enforced"
    return 1
  else
    ok "NetworkPolicy blocks stdlib chamber egress"
  fi

  # L7 DNS allowlist holds: stdlib must NOT resolve arbitrary names (the DNS-tunnel
  # exfil guard — proves baseline + per-chamber CNP compose without L4-shadows-L7).
  # Best-effort: skip cleanly if the chamber image lacks nslookup.
  if kubectl exec -n "$NAMESPACE" "$task_pod" -- sh -c 'command -v nslookup' >/dev/null 2>&1; then
    if kubectl exec -n "$NAMESPACE" "$task_pod" -- nslookup example.com >/dev/null 2>&1; then
      warn "stdlib resolved example.com — L7 DNS allowlist NOT enforced"
      return 1
    else
      ok "L7 DNS allowlist blocks arbitrary name resolution"
    fi
  else
    warn "nslookup absent in chamber image — skipping L7 DNS probe (wget check still covers egress containment)"
  fi

  if kubectl exec -n "$NAMESPACE" "$task_pod" -- \
       cat /run/secrets/llm/api-key >/dev/null 2>&1; then
    warn "/run/secrets/llm/api-key exists inside stdlib chamber pod — credential leak"
    return 1
  else
    ok "Credential isolation (no LLM key in stdlib chamber pod)"
  fi

  if kubectl get serviceaccounts -n "$NAMESPACE" -l sycophant.md/type=workspace-sa -o name \
       | grep -q sa-hello-world; then
    ok "Workspace ServiceAccounts present"
  else
    warn "sa-hello-world ServiceAccount missing"
    return 1
  fi
}

step_7_flutter() {
  step "Step 7: Flutter app demo"

  local enrollment_code
  enrollment_code="$(kubectl get enr "$CLIENT_NAME" -n "$NAMESPACE" -o jsonpath='{.status.enrollmentCode}')"

  printf 'Tailscale loopback IP address: 10.0.2.2:9091\n'
  printf 'Namespace: %s\n' "$NAMESPACE"
  printf 'Enrollment code: %s\n' "$enrollment_code"
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
  printf '\n\033[1;32m==> e2e complete\033[0m\n'
}

main "$@"

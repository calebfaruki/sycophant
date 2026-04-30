# CLI End-to-End Test Guide

Test the syco CLI with a local init in a temp directory.
Workspaces run as agent-sandbox Sandbox CRs with gVisor kernel isolation.

## Prerequisites

- Docker Desktop with Kubernetes enabled (Kind mode)
- Cilium CNI installed (`cilium install`)
- Agent Sandbox v0.3.10 installed
- gVisor (`runsc`) installed in containerd
- `kubectl`, `helm`, `grpcurl` installed
- `ANTHROPIC_API_KEY` set in environment
- Rust toolchain with `aarch64-unknown-linux-musl` target

## Step 0: Preflight

Docker Desktop recreates the cluster on restart, which can wipe
Cilium pods, CRDs, gVisor binaries, and containerd registry config.

```sh
# Check: Cilium CRD
kubectl get crd ciliumnetworkpolicies.cilium.io
# Fix: cilium install && kubectl wait --for=condition=ready \
#   pod -l app.kubernetes.io/part-of=cilium -n kube-system --timeout=180s

# Check: Agent Sandbox controller
kubectl get crd sandboxes.agents.x-k8s.io
# Fix:
#   kubectl apply -f https://github.com/kubernetes-sigs/agent-sandbox/releases/download/v0.3.10/manifest.yaml
#   kubectl apply -f https://github.com/kubernetes-sigs/agent-sandbox/releases/download/v0.3.10/extensions.yaml

# Check: gVisor runsc binary
docker exec desktop-control-plane /usr/local/bin/runsc --version
# Fix:
#   docker exec desktop-control-plane sh -c '
#     curl -fsSL -o /usr/local/bin/runsc \
#       https://storage.googleapis.com/gvisor/releases/release/latest/aarch64/runsc
#     curl -fsSL -o /usr/local/bin/containerd-shim-runsc-v1 \
#       https://storage.googleapis.com/gvisor/releases/release/latest/aarch64/containerd-shim-runsc-v1
#     chmod +x /usr/local/bin/runsc /usr/local/bin/containerd-shim-runsc-v1
#   '

# Check: gVisor containerd config (must include pod_annotations for mount hints)
docker exec desktop-control-plane grep -q runsc /etc/containerd/config.toml
# Fix:
#   docker exec desktop-control-plane sh -c '
#     cat >> /etc/containerd/config.toml << EOF
#         [plugins."io.containerd.grpc.v1.cri".containerd.runtimes.runsc]
#           runtime_type = "io.containerd.runsc.v1"
#           pod_annotations = ["dev.gvisor.*"]
#   EOF
#     kill -HUP $(pidof containerd)
#   '
#   sleep 3

# Check: gVisor RuntimeClass
kubectl get runtimeclass gvisor
# Fix:
#   kubectl apply -f - << 'EOF'
#   apiVersion: node.k8s.io/v1
#   kind: RuntimeClass
#   metadata:
#     name: gvisor
#   handler: runsc
#   EOF

# Chart CRDs (helm upgrade does NOT update CRDs)
kubectl apply -f charts/sycophant/crds/

# Check: containerd insecure registry (for chamber image pulls in Jobs)
docker exec desktop-control-plane \
  cat /etc/containerd/certs.d/host.docker.internal:5555/hosts.toml
# Fix:
#   docker exec desktop-control-plane mkdir -p \
#     /etc/containerd/certs.d/host.docker.internal:5555
#   docker exec desktop-control-plane sh -c \
#     'cat > /etc/containerd/certs.d/host.docker.internal:5555/hosts.toml << EOF
#   [host."http://host.docker.internal:5555"]
#     capabilities = ["pull", "resolve"]
#     skip_verify = true
#   EOF'
```

## Step 1: Build images

Build syco binary and all container images.

```sh
cd ~/sycophant
cargo build -p syco
export SYCO=$(pwd)/target/debug/syco

# All Rust binaries
cargo build --release --target aarch64-unknown-linux-musl \
  -p tightbeam-controller -p tightbeam-llm-job \
  -p airlock-controller -p airlock-runtime \
  -p transponder -p workspace-tools

# Scratch images (tightbeam + airlock + transponder)
for bin in tightbeam-controller tightbeam-llm-job airlock-controller airlock-runtime transponder; do
  cp target/aarch64-unknown-linux-musl/release/$bin ${bin}-linux-musl-arm64
  docker build -f build/Dockerfile --build-arg BINARY=$bin --build-arg TARGETARCH=arm64 -t ${bin}:local .
  rm ${bin}-linux-musl-arm64
done

# Workspace-tools (alpine, needs git)
cp target/aarch64-unknown-linux-musl/release/workspace-tools /tmp/workspace-tools
echo 'FROM alpine:3.21
RUN apk add --no-cache git
COPY --chmod=755 workspace-tools /usr/local/bin/workspace-tools
ENTRYPOINT ["workspace-tools"]' > /tmp/Dockerfile.workspace-tools
docker build -f /tmp/Dockerfile.workspace-tools -t sycophant-workspace-tools:local /tmp/
rm /tmp/workspace-tools /tmp/Dockerfile.workspace-tools

# Chamber images (need airlock-runtime in build context)
cp target/aarch64-unknown-linux-musl/release/airlock-runtime images/git/airlock-runtime-linux-arm64
docker build --build-arg TARGETARCH=arm64 -f images/git/Dockerfile images/git/ -t airlock-git:local
rm images/git/airlock-runtime-linux-arm64

cp target/aarch64-unknown-linux-musl/release/airlock-runtime examples/scenarios/ssh-secret/airlock-runtime-linux-arm64
docker build --build-arg TARGETARCH=arm64 examples/scenarios/ssh-secret/ -t airlock-ssh:local
rm examples/scenarios/ssh-secret/airlock-runtime-linux-arm64
```

Load images into the Kind cluster:

```sh
for img in tightbeam-controller:local tightbeam-llm-job:local \
           airlock-controller:local sycophant-transponder:local \
           sycophant-workspace-tools:local; do
  docker save "$img" | docker exec -i desktop-control-plane ctr -n k8s.io images import --no-unpack -
done
```

Start a local registry for chamber images:

```sh
docker run -d --name e2e-registry -p 5555:5000 registry:2

for img in airlock-git airlock-ssh; do
  docker tag ${img}:local localhost:5555/${img}:latest
  docker push localhost:5555/${img}:latest
done
```

## Step 2: Configure

```sh
cd /tmp && rm -rf syco-e2e && mkdir syco-e2e && cd syco-e2e

$SYCO init local

echo "$ANTHROPIC_API_KEY" | $SYCO secret set anthropic-api-key

$SYCO model set claude-haiku-4-5-20251001 \
  --provider anthropic \
  --secret anthropic-api-key

$SYCO model set claude-sonnet-4-20250514 \
  --provider anthropic \
  --secret anthropic-api-key

$SYCO agent set hello-world \
  --model anthropic.claude-haiku-4-5-20251001 \
  --prompt examples/prompts/hello-world

$SYCO agent set alice \
  --model anthropic.claude-haiku-4-5-20251001 \
  --prompt examples/prompts/alice \
  --description "Friendly and creative. Good with brainstorming, explanations, and people questions."

$SYCO agent set bob \
  --model anthropic.claude-sonnet-4-20250514 \
  --prompt examples/prompts/bob \
  --description "Technical and precise. Good with code, debugging, and system design."

$SYCO workspace create hello-world --image sycophant-workspace-tools:local
$SYCO workspace add-agent hello-world hello-world

$SYCO workspace create multi-agent --image sycophant-workspace-tools:local
$SYCO workspace add-agent multi-agent alice
$SYCO workspace add-agent multi-agent bob

kubectl create namespace syco-e2e --dry-run=client -o yaml | kubectl apply -f -
kubectl apply -f examples/scenarios/ssh-secret/fixtures/ -n syco-e2e
```

Verify:
```sh
$SYCO model list
$SYCO agent list
$SYCO workspace show hello-world
$SYCO workspace show multi-agent
$SYCO secret list
```

## Step 3: Deploy

Append local image overrides and deploy:

```sh
# The CLI generated values.yaml with models, agents, and workspaces.
# Overwrite it with the merged version that adds image overrides,
# workspace chambers/pullPolicy, and chamber definitions.
cat > values.yaml << 'EOF'
models:
  anthropic.claude-haiku-4-5-20251001:
    format: anthropic
    model: claude-haiku-4-5-20251001
    baseUrl: https://api.anthropic.com/v1
    secret:
      name: anthropic-api-key
      env: API_KEY
  anthropic.claude-sonnet-4-20250514:
    format: anthropic
    model: claude-sonnet-4-20250514
    baseUrl: https://api.anthropic.com/v1
    secret:
      name: anthropic-api-key
      env: API_KEY

agents:
  hello-world:
    model: anthropic.claude-haiku-4-5-20251001
    prompt:
      path: examples/prompts/hello-world
  alice:
    model: anthropic.claude-haiku-4-5-20251001
    prompt:
      path: examples/prompts/alice
    description: Friendly and creative. Good with brainstorming, explanations, and people questions.
  bob:
    model: anthropic.claude-sonnet-4-20250514
    prompt:
      path: examples/prompts/bob
    description: Technical and precise. Good with code, debugging, and system design.

workspaces:
  hello-world:
    image: sycophant-workspace-tools
    tag: local
    pullPolicy: Never
    agents:
      - hello-world
    chambers:
      - workspace-ro
      - ssh-secret
  multi-agent:
    image: sycophant-workspace-tools
    tag: local
    pullPolicy: Never
    agents:
      - alice
      - bob
    chambers:
      - workspace-ro

controller:
  image: tightbeam-controller
  tag: local
  pullPolicy: Never
  llmJobImage: tightbeam-llm-job:local

airlock:
  image: airlock-controller
  tag: local
  pullPolicy: Never

transponder:
  image: sycophant-transponder
  tag: local
  pullPolicy: Never

chambers:
  workspace-ro:
    image: host.docker.internal:5555/airlock-git:latest
    workspaceMode: readOnly
    workspaceMountPath: /workspace
  ssh-secret:
    image: host.docker.internal:5555/airlock-ssh:latest
    workspaceMode: readOnly
    workspaceMountPath: /workspace
    credentials:
      - secret: demo-ssh-key
        file: /root/.ssh/id_ed25519
EOF

$SYCO up
```

Expected: `Prompt 'hello-world' applied.`, `Prompt 'alice' applied.`,
`Prompt 'bob' applied.` followed by helm output.

## Step 4: Verify

```sh
kubectl get sandbox -n syco-e2e
kubectl get pods -n syco-e2e
kubectl get tightbeammodels -n syco-e2e
kubectl logs -n syco-e2e hello-world -c transponder
kubectl logs -n syco-e2e deployment/airlock-controller
```

Expected:
- Sandbox CRs `hello-world` and `multi-agent` exist (workspaces run as
  agent-sandbox Sandbox CRs with gVisor kernel isolation)
- All pods running (workspace pods show 2/2: transponder + workspace-tools)
- Models registered
- Transponder: `connected to tightbeam controller`, `tool router initialized, count=N`, `running single-agent mode`
- Airlock: `discovered tools from image`, `chamber watcher initial sync complete, tool_count=N`

## Step 5: Chat

```sh
echo "Use the ssh tool to run: cat /home/agent/.ssh/id_ed25519" | $SYCO chat hello-world
```

Expected: JSON response printed to stdout with `sendMessage.content[].text`
containing the LLM's reply. The response arrives after 10-30 seconds
(cold LLM Job startup + API call + tool execution). The LLM should call
the ssh tool. The chamber has a demo SSH key staged to
`/home/agent/.ssh/id_ed25519`. If no output appears, check troubleshooting.

## Step 6: Verify security

### gVisor kernel isolation

```sh
kubectl exec -n syco-e2e hello-world -c workspace-tools -- dmesg | head -1
```

Expected: `Starting gVisor...` — confirms the workspace runs under
gVisor's sandboxed kernel, not the host kernel.

### Secret scrubbing

```sh
kubectl logs -n syco-e2e hello-world -c transponder | grep -c "FAKE-ED25519-PRIVATE-KEY"
```

Expected: 0. The scrubber replaces it with `[REDACTED:demo-ssh-key]`.

### Tool execution

```sh
kubectl logs -n syco-e2e deployment/airlock-controller | grep "received tool result"
```

Expected: `received tool result, call_id=..., exit_code=0`

### NetworkPolicy enforcement

```sh
kubectl exec -n syco-e2e hello-world -c workspace-tools -- \
  wget -qO- --timeout=3 https://httpbin.org/ip 2>&1
```

Expected: timeout. Workspace has no internet access.

### Credential isolation

```sh
kubectl exec -n syco-e2e hello-world -c workspace-tools -- \
  cat /run/secrets/llm/api-key 2>&1
```

Expected: "No such file or directory". No secrets mounted in workspace.

### Workspace scoping

```sh
kubectl get serviceaccounts -n syco-e2e -l sycophant.io/type=workspace-sa
kubectl exec -n syco-e2e hello-world -c transponder -- \
  ls /var/run/secrets/kubernetes.io/serviceaccount/token
kubectl logs -n syco-e2e deployment/airlock-controller | grep "workspace bindings"
```

Expected:
- ServiceAccounts `sa-hello-world` and `sa-multi-agent` exist
- SA token file is mounted in the transponder container
- Controller log shows `loaded workspace bindings`

## Step 7: Teardown

```sh
$SYCO down
$SYCO workspace delete multi-agent
$SYCO workspace delete hello-world
$SYCO agent delete hello-world
$SYCO agent delete alice
$SYCO agent delete bob
$SYCO model delete anthropic.claude-haiku-4-5-20251001
$SYCO model delete anthropic.claude-sonnet-4-20250514
$SYCO secret delete anthropic-api-key
```

Verify idempotency:
```sh
$SYCO down
```

Expected: `Not running.`

## Step 8: Cleanup

```sh
cd /tmp && rm -rf syco-e2e
docker rm -f e2e-registry 2>/dev/null
```

## Troubleshooting

### Transponder CrashLoopBackOff
```sh
kubectl logs -n syco-e2e hello-world -c transponder --previous
```
- "subscribe stream closed": Controller restarted. Transponder will
  reconnect on next restart.
- "transport error" retries then fails: Controller unreachable. Check
  `kubectl get svc -n syco-e2e` and `kubectl get endpoints -n syco-e2e`.

### Airlock controller not ready
```sh
kubectl logs -n syco-e2e deployment/airlock-controller
```
- "no k8s client available": ServiceAccount or RBAC misconfigured.
  Check `kubectl get sa -n syco-e2e` and ClusterRoleBinding.
- "watcher kube client failed": Can't connect to Kubernetes API.
  Check RBAC for `airlock.dev/airlockchambers` watch permission.

### Conversation corruption (API error 400: tool_use without tool_result)
The tightbeam controller persists conversation logs to a PVC. If a
previous run left an orphaned tool_use block (from a failed tool call),
every subsequent turn fails with:
```
tool_use ids were found without tool_result blocks
```

Fix: delete PVCs and restart the controller.
```sh
kubectl delete pvc --all -n syco-e2e
kubectl rollout restart deployment tightbeam-controller -n syco-e2e
```

### Turn stuck (no response after "received inbound message")
Check controller trace:
```sh
kubectl logs -n syco-e2e deployment/tightbeam-controller
```
- No `turn: entry`: Transponder didn't send the Turn. Check transponder
  logs for errors.
- `enqueue_turn: complete` but no `wait_for_turn: recv complete`: No LLM
  Job connected. Check `kubectl get jobs -n syco-e2e` and Job logs.
- `get_turn: received assignment` but no `stream_turn_result`: LLM Job
  got the assignment but API call is slow or failing. Check Job logs.

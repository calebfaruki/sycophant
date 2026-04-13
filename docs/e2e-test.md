# DevOps End-to-End Test Guide

Test the sycophant Helm chart with locally built images.

## Prerequisites

- Docker Desktop with Kubernetes enabled (Kind mode)
- Cilium CNI installed (`cilium install`)
- `kubectl`, `helm`, `grpcurl` installed
- `ANTHROPIC_API_KEY` set in environment
- Rust toolchain with `aarch64-unknown-linux-musl` target

## Step 0: Preflight

Docker Desktop recreates the cluster on restart, which can wipe
Cilium pods, CRDs, and containerd registry config.

```sh
# Cilium: CRD must exist (cilium status lies when pods are gone)
kubectl get crd ciliumnetworkpolicies.cilium.io
# If not found: cilium install && kubectl wait --for=condition=ready \
#   pod -l app.kubernetes.io/part-of=cilium -n kube-system --timeout=180s

# Chart CRDs: helm upgrade does NOT update CRDs, so always reapply
kubectl apply -f charts/sycophant/crds/

# Containerd insecure registry config (for chamber image pulls in Jobs)
docker exec desktop-control-plane \
  cat /etc/containerd/certs.d/host.docker.internal:5555/hosts.toml
# If not found:
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

Cross-compile all binaries and build Docker images locally.

```sh
cd ~/sycophant

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

Start a local registry for chamber images (airlock reads OCI labels via HTTP):

```sh
docker run -d --name e2e-registry -p 5555:5000 registry:2

for img in airlock-git airlock-ssh; do
  docker tag ${img}:local localhost:5555/${img}:latest
  docker push localhost:5555/${img}:latest
done
```

## Step 2: Configure

Create namespace, prompt ConfigMaps, and secrets.

```sh
kubectl create namespace e2e-test --dry-run=client -o yaml | kubectl apply -f -

kubectl create configmap sycophant-prompt-hello-world \
  --namespace e2e-test \
  --from-file=examples/prompts/hello-world/ \
  --dry-run=client -o yaml | kubectl apply -f -

kubectl create configmap sycophant-prompt-alice \
  --namespace e2e-test \
  --from-file=examples/prompts/alice/ \
  --dry-run=client -o yaml | kubectl apply -f -

kubectl create configmap sycophant-prompt-bob \
  --namespace e2e-test \
  --from-file=examples/prompts/bob/ \
  --dry-run=client -o yaml | kubectl apply -f -

kubectl create secret generic sycophant-llm-anthropic \
  --namespace e2e-test \
  --from-literal=sycophant-llm-anthropic="$ANTHROPIC_API_KEY" \
  --dry-run=client -o yaml | kubectl apply -f -

kubectl apply -f examples/scenarios/ssh-secret/fixtures/ -n e2e-test
```

## Step 3: Deploy

```sh
helm upgrade --install e2e-test charts/sycophant/ \
  -n e2e-test \
  -f examples/scenarios/hello-world/values.yaml \
  -f examples/scenarios/ssh-secret/values.yaml \
  -f examples/scenarios/multi-agent/values.yaml \
  -f docs/e2e/values.yaml \
  --wait
```

`--wait` blocks until all pods pass readiness probes.

## Step 4: Verify

```sh
kubectl get pods -n e2e-test
kubectl get tightbeammodels -n e2e-test
kubectl logs -n e2e-test deployment/hello-world -c transponder
kubectl logs -n e2e-test deployment/airlock-controller
```

Expected:
- All pods running
- Models registered
- Transponder: `connected to tightbeam controller`, `tool router initialized, count=N`, `running single-agent mode`
- Airlock: `discovered tools from image`, `chamber watcher initial sync complete, tool_count=N`

## Step 5: Chat

```sh
kubectl port-forward -n e2e-test svc/tightbeam-controller 9090:9090 &
sleep 2

grpcurl -plaintext -max-time 60 -d '{"register":{"channel_type":"test","channel_name":"e2e","workspace":"hello-world"}}
{"user_message":{"content":[{"text":{"text":"Use the ssh tool to run: cat /root/.ssh/id_ed25519"}}],"sender":"tester"}}' \
  localhost:9090 tightbeam.v1.TightbeamController/ChannelStream

kill %1
```

The LLM should call the ssh tool. The chamber has a demo SSH key
mounted at `/root/.ssh/id_ed25519`.

## Step 6: Verify security

### Secret scrubbing

```sh
kubectl logs -n e2e-test deployment/hello-world -c transponder | grep -c "FAKE-ED25519-PRIVATE-KEY"
```

Expected: 0. The scrubber replaces it with `[REDACTED:demo-ssh-key]`.

### Tool execution

```sh
kubectl logs -n e2e-test deployment/airlock-controller | grep "received tool result"
```

Expected: `received tool result, call_id=..., exit_code=0`

### NetworkPolicy enforcement

```sh
kubectl exec -n e2e-test deployment/hello-world -c workspace-tools -- \
  wget -qO- --timeout=3 https://httpbin.org/ip 2>&1
```

Expected: timeout. Workspace has no internet access.

### Credential isolation

```sh
kubectl exec -n e2e-test deployment/hello-world -c workspace-tools -- \
  cat /run/secrets/llm/api-key 2>&1
```

Expected: "No such file or directory". No secrets mounted in workspace.

### Workspace scoping

```sh
kubectl get serviceaccounts -n e2e-test -l sycophant.io/type=workspace-sa
kubectl exec -n e2e-test deployment/hello-world -c transponder -- \
  ls /var/run/secrets/kubernetes.io/serviceaccount/token
kubectl logs -n e2e-test deployment/airlock-controller | grep "workspace bindings"
```

Expected:
- ServiceAccounts `sa-hello-world` and `sa-multi-agent` exist
- SA token file is mounted in the transponder container
- Controller log shows `loaded workspace bindings`

## Step 7: Teardown

```sh
helm uninstall e2e-test --namespace e2e-test
kubectl delete namespace e2e-test
```

## Step 8: Cleanup

```sh
docker rm -f e2e-registry 2>/dev/null
```

## Troubleshooting

### Transponder CrashLoopBackOff
```sh
kubectl logs -n e2e-test deployment/hello-world -c transponder --previous
```
- "subscribe stream closed": Controller restarted. Transponder will
  reconnect on next restart.
- "transport error" retries then fails: Controller unreachable. Check
  `kubectl get svc -n e2e-test` and `kubectl get endpoints -n e2e-test`.

### Airlock controller not ready
```sh
kubectl logs -n e2e-test deployment/airlock-controller
```
- "no k8s client available": ServiceAccount or RBAC misconfigured.
  Check `kubectl get sa -n e2e-test` and ClusterRoleBinding.
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
kubectl delete pvc --all -n e2e-test
kubectl rollout restart deployment tightbeam-controller -n e2e-test
```

### Turn stuck (no response after "received inbound message")
Check controller trace:
```sh
kubectl logs -n e2e-test deployment/tightbeam-controller
```
- No `turn: entry`: Transponder didn't send the Turn. Check transponder
  logs for errors.
- `enqueue_turn: complete` but no `wait_for_turn: recv complete`: No LLM
  Job connected. Check `kubectl get jobs -n e2e-test` and Job logs.
- `get_turn: received assignment` but no `stream_turn_result`: LLM Job
  got the assignment but API call is slow or failing. Check Job logs.

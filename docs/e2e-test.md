# End-to-End Test Guide

Run the full tightbeam + transponder + workspace-tools stack on local
Docker Desktop Kubernetes.

## Prerequisites

- Docker Desktop with Kubernetes enabled (Kind mode)
- `kubectl`, `helm`, `grpcurl` installed
- `aarch64-linux-musl-gcc` installed (`brew install filosottile/musl-cross/musl-cross`)
- `~/.cargo/config.toml` has:
  ```toml
  [target.aarch64-unknown-linux-musl]
  linker = "aarch64-linux-musl-gcc"
  ```
- Sibling repos cloned:
  ```
  ~/tightbeam/
  ~/airlock/
  ~/sychophant/
  ```
- `ANTHROPIC_API_KEY` set in environment

## Step 1: Build binaries

```sh
cd ~/tightbeam && cargo build --release --target aarch64-unknown-linux-musl \
  -p tightbeam-controller -p tightbeam-llm-job &

cd ~/airlock && cargo build --release --target aarch64-unknown-linux-musl \
  -p airlock-controller -p airlock-agent &

cd ~/sychophant && cargo build --release --target aarch64-unknown-linux-musl \
  -p transponder -p workspace-tools &

wait
```

## Step 2: Build Docker images

```sh
# Tightbeam
cd ~/tightbeam
cp target/aarch64-unknown-linux-musl/release/tightbeam-controller \
  tightbeam-controller-linux-musl-arm64
cp target/aarch64-unknown-linux-musl/release/tightbeam-llm-job \
  tightbeam-llm-job-linux-musl-arm64
docker build --build-arg TARGETARCH=arm64 -t tightbeam-controller:dev \
  -f Dockerfile.controller .
docker build --build-arg TARGETARCH=arm64 -t tightbeam-llm-job:dev \
  -f Dockerfile.llm-job .
rm tightbeam-*-linux-musl-arm64

# Airlock
cd ~/airlock
cp target/aarch64-unknown-linux-musl/release/airlock-controller \
  airlock-controller-linux-musl-arm64
cp target/aarch64-unknown-linux-musl/release/airlock-agent \
  airlock-agent-linux-arm64
cp airlock-agent-linux-arm64 images/git/
docker build --build-arg TARGETARCH=arm64 -t airlock-controller:dev \
  -f Dockerfile.controller .
docker build --build-arg TARGETARCH=arm64 -t airlock-agent:dev \
  -f Dockerfile.agent .
docker build --build-arg TARGETARCH=arm64 -t airlock-git:dev \
  -f images/git/Dockerfile images/git/
rm airlock-*-linux-*arm64 images/git/airlock-*

# Sycophant
cd ~/sychophant
cp target/aarch64-unknown-linux-musl/release/transponder .
cp target/aarch64-unknown-linux-musl/release/workspace-tools .
echo 'FROM scratch
COPY transponder /usr/local/bin/transponder
ENTRYPOINT ["transponder"]' | docker build -t transponder:dev -f - .
echo 'FROM alpine:3.21
RUN apk add --no-cache git
COPY workspace-tools /usr/local/bin/workspace-tools
ENTRYPOINT ["workspace-tools"]' | docker build -t workspace-tools:dev -f - .
rm transponder workspace-tools
```

## Step 3: Load images into Kind node

```sh
for img in tightbeam-controller:dev tightbeam-llm-job:dev \
           airlock-controller:dev airlock-agent:dev airlock-git:dev \
           transponder:dev workspace-tools:dev; do
  docker exec desktop-control-plane \
    ctr --namespace k8s.io images rm "docker.io/library/$img" 2>/dev/null
  docker save "$img" | docker exec -i desktop-control-plane \
    ctr --namespace k8s.io images import -
done
```

Always `ctr images rm` before `import`. Without it, ctr silently skips
reimports when the tag already exists.

## Step 4: Create namespace and CRDs

```sh
kubectl create namespace e2e-test
kubectl apply -f ~/tightbeam/deploy/crds/
kubectl apply -f ~/airlock/deploy/crds/
```

## Step 5: Create secrets

```sh
kubectl create secret generic sycophant-llm-anthropic \
  --namespace e2e-test \
  --from-literal=provider=anthropic \
  --from-literal=model=claude-sonnet-4-20250514 \
  --from-literal=api-key="$ANTHROPIC_API_KEY" \
  --from-literal=max-tokens=8192
```

## Step 6: Create agent prompt

```sh
kubectl apply -f - <<'EOF'
apiVersion: v1
kind: ConfigMap
metadata:
  name: sycophant-agent-e2e-ws-test-agent
  namespace: e2e-test
data:
  prompt.md: |
    You are a test agent. Keep responses brief. One sentence max.
EOF
```

## Step 7: Deploy with Helm

```sh
cd ~/sychophant
helm install e2e-test charts/sycophant/ \
  --namespace e2e-test \
  --set controller.image=tightbeam-controller \
  --set controller.tag=dev \
  --set controller.pullPolicy=Never \
  --set transponder.image=transponder \
  --set transponder.tag=dev \
  --set transponder.pullPolicy=Never \
  --set llm=anthropic \
  --set 'workspaces.e2e-ws.image=workspace-tools' \
  --set 'workspaces.e2e-ws.tag=dev' \
  --set 'workspaces.e2e-ws.pullPolicy=Never' \
  --set 'workspaces.e2e-ws.agents[0].name=test-agent'
```

Wait for pods:
```sh
kubectl get pods -n e2e-test -w
```

Wait until `sycophant-controller` (1/1) and `e2e-ws` (2/2) are Running.
All components retry connections automatically — ordering doesn't matter.

## Step 8: Send a message

```sh
kubectl port-forward -n e2e-test svc/sycophant-controller 9090:9090 &

grpcurl -plaintext \
  -import-path ~/tightbeam/crates/tightbeam-proto/proto \
  -proto tightbeam/v1/tightbeam.proto \
  -d '{"register":{"channel_type":"test","channel_name":"e2e"}}
{"user_message":{"content":[{"text":{"text":"Say hello"}}],"sender":"tester"}}' \
  localhost:9090 tightbeam.v1.TightbeamController/ChannelStream

kill %1
```

The controller auto-creates an LLM Job when the Turn arrives. No
manual Job creation needed.

## Step 9: Verify

### Message pipeline

```sh
kubectl logs -n e2e-test deployment/e2e-ws -c transponder
```

Expected:
```
connected to tightbeam controller
connected to workspace tools
tool router initialized, count=4
subscribed to tightbeam for inbound messages
running single-agent mode, agent=test-agent
received inbound message, sender=tester
```

```sh
kubectl logs -n e2e-test deployment/sycophant-controller
```

Expected trace:
```
turn: entry
turn: no LLM Job connected, creating one
created LLM Job tightbeam-llm-default-...
turn: LLM Job created
turn: waiting for Job to connect
get_turn: marking job connected
turn: enqueueing turn
enqueue_turn: complete, ok=true
wait_for_turn: recv complete, got=true
get_turn: received assignment with 1 messages
stream_turn_result: entry
take_active_result_tx: found=true
```

### LLM Job auto-created

```sh
kubectl get jobs -n e2e-test
```

Expected: one Job named `tightbeam-llm-default-*` in Running status.

### NetworkPolicy enforcement

```sh
kubectl exec -n e2e-test deployment/e2e-ws -c workspace-tools -- \
  wget -qO- --timeout=3 https://httpbin.org/ip 2>&1
```

Expected: timeout. Workspace has no internet access.

### Credential isolation

```sh
kubectl exec -n e2e-test deployment/e2e-ws -c workspace-tools -- \
  cat /run/secrets/llm/api-key 2>&1
```

Expected: "No such file or directory". No secrets mounted in workspace.

## Teardown

```sh
helm uninstall e2e-test --namespace e2e-test
kubectl delete namespace e2e-test
kubectl delete clusterrole e2e-test-controller e2e-test-chart-admin
kubectl delete clusterrolebinding e2e-test-controller e2e-test-chart-admin
kubectl delete -f ~/tightbeam/deploy/crds/
kubectl delete -f ~/airlock/deploy/crds/
```

## Troubleshooting

### Transponder CrashLoopBackOff
```sh
kubectl logs -n e2e-test deployment/e2e-ws -c transponder --previous
```
- "subscribe stream closed": Controller restarted. Transponder will
  reconnect on next restart.
- "transport error" retries then fails: Controller unreachable. Check
  `kubectl get svc -n e2e-test` and `kubectl get endpoints -n e2e-test`.

### Turn stuck (no response after "received inbound message")
Check controller trace:
```sh
kubectl logs -n e2e-test deployment/sycophant-controller
```
- No `turn: entry`: Transponder didn't send the Turn. Check transponder
  logs for errors.
- `enqueue_turn: complete` but no `wait_for_turn: recv complete`: No LLM
  Job connected. Check `kubectl get jobs -n e2e-test` and Job logs.
- `get_turn: received assignment` but no `stream_turn_result`: LLM Job
  got the assignment but API call is slow or failing. Check Job logs.

### Stale images after rebuild
Always delete before reimporting:
```sh
docker exec desktop-control-plane \
  ctr --namespace k8s.io images rm docker.io/library/<image>:dev
docker save <image>:dev | docker exec -i desktop-control-plane \
  ctr --namespace k8s.io images import -
```

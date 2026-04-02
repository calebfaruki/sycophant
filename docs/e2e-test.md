# End-to-End Test Guide

Test the sycophant Helm chart with locally built images.

## Prerequisites

- Docker Desktop with Kubernetes enabled (Kind mode)
- Cilium CNI installed (`cilium install`)
- `kubectl`, `helm`, `grpcurl` installed
- `ANTHROPIC_API_KEY` set in environment
- Rust toolchain with `aarch64-unknown-linux-musl` target

## Step 0: Build images

Cross-compile all binaries and build Docker images locally.

```sh
# Tightbeam
cd ~/tightbeam
cargo build --release --target aarch64-unknown-linux-musl -p tightbeam-controller -p tightbeam-llm-job
cp target/aarch64-unknown-linux-musl/release/tightbeam-controller tightbeam-controller-linux-musl-arm64
cp target/aarch64-unknown-linux-musl/release/tightbeam-llm-job tightbeam-llm-job-linux-musl-arm64
docker build --build-arg BINARY=tightbeam-controller --build-arg TARGETARCH=arm64 -t tightbeam-controller:local .
docker build --build-arg BINARY=tightbeam-llm-job --build-arg TARGETARCH=arm64 -t tightbeam-llm-job:local .
rm tightbeam-controller-linux-musl-arm64 tightbeam-llm-job-linux-musl-arm64

# Airlock
cd ~/airlock
cargo build --release --target aarch64-unknown-linux-musl -p airlock-controller -p airlock-runtime
cp target/aarch64-unknown-linux-musl/release/airlock-controller airlock-controller-linux-musl-arm64
docker build --build-arg TARGETARCH=arm64 -f Dockerfile.controller -t airlock-controller:local .
rm airlock-controller-linux-musl-arm64

# Airlock chamber images (need airlock-runtime binary in build context)
cp target/aarch64-unknown-linux-musl/release/airlock-runtime images/git/airlock-runtime-linux-arm64
docker build --build-arg TARGETARCH=arm64 -f images/git/Dockerfile images/git/ -t airlock-git:local
rm images/git/airlock-runtime-linux-arm64

cp target/aarch64-unknown-linux-musl/release/airlock-runtime ~/sycophant/examples/scenarios/ssh-secret/airlock-runtime-linux-arm64
docker build --build-arg TARGETARCH=arm64 ~/sycophant/examples/scenarios/ssh-secret/ -t airlock-ssh:local
rm ~/sycophant/examples/scenarios/ssh-secret/airlock-runtime-linux-arm64

# Sycophant
cd ~/sycophant
cargo build --release --target aarch64-unknown-linux-musl -p transponder -p workspace-tools

cp target/aarch64-unknown-linux-musl/release/transponder /tmp/transponder
echo 'FROM scratch
COPY --chmod=755 transponder /usr/local/bin/transponder
ENTRYPOINT ["transponder"]' > /tmp/Dockerfile.transponder
docker build -f /tmp/Dockerfile.transponder -t sycophant-transponder:local /tmp/

cp target/aarch64-unknown-linux-musl/release/workspace-tools /tmp/workspace-tools
echo 'FROM alpine:3.21
RUN apk add --no-cache git
COPY --chmod=755 workspace-tools /usr/local/bin/workspace-tools
ENTRYPOINT ["workspace-tools"]' > /tmp/Dockerfile.workspace-tools
docker build -f /tmp/Dockerfile.workspace-tools -t sycophant-workspace-tools:local /tmp/

rm /tmp/transponder /tmp/workspace-tools /tmp/Dockerfile.transponder /tmp/Dockerfile.workspace-tools
```

Load images into the Kind cluster:

```sh
for img in tightbeam-controller:local tightbeam-llm-job:local \
           airlock-controller:local sycophant-transponder:local \
           sycophant-workspace-tools:local; do
  docker save "$img" | docker exec -i desktop-control-plane ctr -n k8s.io images import --no-unpack -
done
```

Start a local registry for chamber images (airlock needs to read OCI labels):

```sh
docker run -d --name e2e-registry -p 5555:5000 registry:2

for img in airlock-git airlock-ssh; do
  docker tag ${img}:local localhost:5555/${img}:latest
  docker push localhost:5555/${img}:latest
done
```

Chamber images are referenced as `host.docker.internal:5555/<image>:latest`
in the e2e values overlay. The airlock controller reads their OCI labels
via HTTP to discover tools.

## Step 1: Clean up previous run

**Critical**: always delete PVCs. The tightbeam controller persists
conversation logs to a PVC. Stale tool_use blocks without matching
tool_result blocks corrupt all subsequent turns.

```sh
helm uninstall e2e-test -n e2e-test 2>/dev/null || true
kubectl delete jobs --all -n e2e-test 2>/dev/null || true
kubectl delete pvc --all -n e2e-test 2>/dev/null || true
kubectl delete tightbeammodels --all -n e2e-test 2>/dev/null || true
kubectl delete crd airlockchambers.airlock.dev tightbeammodels.tightbeam.dev tightbeamchannels.tightbeam.dev 2>/dev/null || true
```

## Step 2: Create namespace and pre-install fixtures

```sh
kubectl create namespace e2e-test --dry-run=client -o yaml | kubectl apply -f -

kubectl create configmap sycophant-agent-hello-world-hello-world \
  --namespace e2e-test \
  --from-file=examples/agents/hello-world/ \
  --dry-run=client -o yaml | kubectl apply -f -

kubectl create configmap sycophant-agent-multi-agent-alice \
  --namespace e2e-test \
  --from-file=examples/agents/alice/ \
  --dry-run=client -o yaml | kubectl apply -f -

kubectl create configmap sycophant-agent-multi-agent-bob \
  --namespace e2e-test \
  --from-file=examples/agents/bob/ \
  --dry-run=client -o yaml | kubectl apply -f -
```

## Step 3: Deploy with Helm

```sh
helm upgrade --install e2e-test charts/sycophant/ \
  -n e2e-test \
  -f examples/scenarios/hello-world/values.yaml \
  -f examples/scenarios/ssh-secret/values.yaml \
  -f examples/scenarios/multi-agent/values.yaml \
  -f docs/e2e/values.yaml \
  --wait
```

`--wait` blocks until all pods pass readiness probes. Both
controllers expose `grpc.health.v1.Health`; workspace-tools uses
an exec probe on the UDS socket.

## Step 4: Create secret and post-install fixtures

The secret is the only resource that needs runtime injection.
Post-install fixtures (TightbeamModel) reference the secret by
name but don't need it until an LLM Job runs.

```sh
kubectl create secret generic sycophant-llm-anthropic \
  --namespace e2e-test \
  --from-literal=api-key="$ANTHROPIC_API_KEY" \
  --dry-run=client -o yaml | kubectl apply -f -

kubectl apply -f docs/e2e/fixtures/post-install/
kubectl apply -f examples/scenarios/ssh-secret/fixtures/ -n e2e-test
```

## Step 5: Send a message

```sh
kubectl port-forward -n e2e-test svc/sycophant-controller 9090:9090 &
sleep 2

grpcurl -plaintext -d '{"register":{"channel_type":"test","channel_name":"e2e"}}
{"user_message":{"content":[{"text":{"text":"Use the ssh tool to run: cat /root/.ssh/id_ed25519"}}],"sender":"tester"}}' \
  localhost:9090 tightbeam.v1.TightbeamController/ChannelStream

kill %1
```

The controller auto-creates an LLM Job when the Turn arrives.
The LLM should call the ssh tool with the `cat` command. The chamber
has a demo SSH key mounted at `/root/.ssh/id_ed25519`.
The scrubber replaces the raw key value with `[REDACTED:demo-ssh-key]`.

## Step 6: Verify

### Message pipeline

```sh
kubectl logs -n e2e-test deployment/hello-world -c transponder
```

Expected:
```
connected to tightbeam controller
connected to workspace tools
connected to airlock controller
tool router initialized, count=N    # N > 0 (workspace tools + airlock tools)
subscribed to tightbeam for inbound messages
running single-agent mode, agent=hello-world
received inbound message, sender=tester
```

```sh
kubectl logs -n e2e-test deployment/sycophant-controller
```

Expected trace:
```
turn: entry
turn: no LLM Job connected, creating one
turn: LLM Job created
turn: waiting for Job to connect
get_turn: marking job connected
turn: conversation lock released
turn: enqueueing turn
enqueue_turn: complete, ok=true
turn: enqueued, returning stream
wait_for_turn: recv complete, got=true
get_turn: received assignment with 1 messages
stream_turn_result: entry
take_active_result_tx: found=true
```

### Airlock controller

```sh
kubectl logs -n e2e-test deployment/airlock-controller
```

Expected:
```
k8s client initialized, Job creation enabled
starting airlock-controller
chamber watcher initialized, clearing registries
discovered tools from image    # one line per chamber with an image
chamber watcher initial sync complete, tool_count=N    # N > 0
watcher initial sync complete, starting gRPC server
```

### LLM Job auto-created

```sh
kubectl get jobs -n e2e-test
```

Expected: one Job named `tightbeam-llm-default-*` in Running status.

### Airlock tool execution and secret scrubbing

The ssh tool Job is ephemeral (TTL 30s) — it may be gone by the time
you check. Verify via the airlock controller logs instead:

```sh
kubectl logs -n e2e-test deployment/airlock-controller | grep "received tool result"
```

Expected: `received tool result, call_id=..., exit_code=0`

Verify the raw SSH key does not appear in transponder logs:

```sh
kubectl logs -n e2e-test deployment/hello-world -c transponder | grep -c "FAKE-ED25519-PRIVATE-KEY"
```

Expected: 0. The scrubber replaces it with `[REDACTED:demo-ssh-key]`.

### Multi-agent routing

```sh
kubectl logs -n e2e-test deployment/multi-agent -c transponder
```

Expected: `running multi-agent mode` (not `single-agent mode`).
After a message is sent, also expect `router selected agent` with
either `alice` or `bob`.

Note: tightbeam broadcasts messages to all subscribers. Both
workspace transponders process every message. This is expected
for e2e testing.

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

## Teardown

```sh
helm uninstall e2e-test --namespace e2e-test
kubectl delete namespace e2e-test
docker rm -f e2e-registry 2>/dev/null
```

If namespace deletion hangs, a CRD finalizer is blocking it.
Delete the CRD instances first (`kubectl delete tightbeammodels --all -n e2e-test`),
then retry the namespace deletion.

Optionally delete CRDs. Helm installs CRDs but intentionally does
not delete them on uninstall (to protect user data). Deleting a CRD
deletes **all instances of that type cluster-wide**, not just in the
test namespace. Only do this on a dedicated test cluster:

```sh
kubectl delete -f charts/sycophant/crds/
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
kubectl rollout restart deployment sycophant-controller -n e2e-test
```

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

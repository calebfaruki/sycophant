# End-to-End Test Guide

Test the sycophant Helm chart with released images from GHCR.

## Prerequisites

The Helm chart installs cluster-scoped CRDs and ClusterRoles.
Multiple installations in different namespaces on the same cluster
are safe — CRDs are idempotent and ClusterRoles are release-scoped.
The only risk is the optional CRD deletion in teardown, which
removes all instances cluster-wide (see Teardown section).

- Docker Desktop with Kubernetes enabled (Kind mode)
- Cilium CNI installed (`cilium install`)
- `kubectl`, `helm`, `grpcurl` installed
- `ANTHROPIC_API_KEY` set in environment

## Step 1: Clean up previous run

Safe to run on a clean cluster. Removes leftover Jobs, PVCs, and
CRD instances from a failed or incomplete previous run.

```sh
kubectl delete jobs --all -n e2e-test 2>/dev/null || true
kubectl delete pvc --all -n e2e-test 2>/dev/null || true
kubectl delete airlocktools --all -n e2e-test 2>/dev/null || true
kubectl delete tightbeammodels --all -n e2e-test 2>/dev/null || true
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
  -f examples/scenarios/safe-secrets/values.yaml \
  -f examples/scenarios/multi-agent/values.yaml \
  -f docs/e2e/values.yaml \
  --wait
```

`--wait` blocks until all pods pass readiness probes. Both
controllers expose `grpc.health.v1.Health`; workspace-tools uses
an exec probe on the UDS socket.

## Step 4: Create secret and post-install fixtures

The secret is the only resource that needs runtime injection.
Post-install fixtures (TightbeamModel, AirlockTool) reference
the secret by name but don't need it until an LLM Job runs.

```sh
cat <<EOF > /tmp/sycophant-llm.env
provider=anthropic
model=claude-sonnet-4-20250514
api-key=${ANTHROPIC_API_KEY}
max-tokens=8192
EOF

kubectl create secret generic sycophant-llm-anthropic \
  --namespace e2e-test \
  --from-env-file=/tmp/sycophant-llm.env \
  --dry-run=client -o yaml | kubectl apply -f -

rm /tmp/sycophant-llm.env

kubectl apply -f docs/e2e/fixtures/post-install/
kubectl apply -f examples/scenarios/safe-secrets/fixtures/ -n e2e-test
```

## Step 5: Send a message

```sh
kubectl port-forward -n e2e-test svc/sycophant-controller 9090:9090 &
sleep 2

grpcurl -plaintext -d '{"register":{"channel_type":"test","channel_name":"e2e"}}
{"user_message":{"content":[{"text":{"text":"Use the echo-secret tool"}}],"sender":"tester"}}' \
  localhost:9090 tightbeam.v1.TightbeamController/ChannelStream

kill %1
```

The controller auto-creates an LLM Job when the Turn arrives.
The LLM should call the echo-secret tool, which runs `printenv
DUMMY_TOKEN` in a chamber with the dummy secret. The scrubber
replaces the raw value with `[REDACTED:e2e-dummy-secret]`.

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
tool router initialized, count=6
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
```

### LLM Job auto-created

```sh
kubectl get jobs -n e2e-test
```

Expected: one Job named `tightbeam-llm-default-*` in Running status.

### Airlock tool execution and secret scrubbing

```sh
kubectl get jobs -n e2e-test | grep echo-secret
```

Expected: one Job with `echo-secret` in the name.

Verify the raw secret does not appear in transponder logs:

```sh
kubectl logs -n e2e-test deployment/hello-world -c transponder | grep -c "super-secret-value-12345"
```

Expected: 0. The scrubber replaces it with `[REDACTED:e2e-dummy-secret]`.

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
```

If namespace deletion hangs, a CRD finalizer is blocking it.
Delete the CRD instances first (`kubectl delete airlocktools,tightbeammodels --all -n e2e-test`),
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
  Check RBAC for `airlock.dev/airlocktools` watch permission.

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

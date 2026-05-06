# DevOps End-to-End Test Guide

Test the sycophant Helm chart with locally built images.
Workspaces run as agent-sandbox Sandbox CRs with gVisor kernel isolation.

## Prerequisites

- Docker Desktop running (with bundled Kubernetes **disabled** — see Step 0)
- `k3d` v5.8.3+ installed (`brew install k3d`)
- `kubectl`, `helm`, `grpcurl` installed
- `MISTRAL_API_KEY` set in environment (default model is `mistral-small-latest`)
- `ANTHROPIC_API_KEY` set in environment (alternate models, also used by some scenarios)
- Rust toolchain with `aarch64-unknown-linux-musl` target

The cluster runs on k3d (k3s in Docker). This is the supported runtime for sycophant local self-host because the bundled-Versitygw mode requires the cluster to see your host filesystem, and Docker Desktop's bundled k8s does not expose `/Users` to its kind node.

## Step 0: Bootstrap k3d cluster

A clean cluster bootstrap covers: k3d cluster create, Cilium CNI, gVisor runtime, Agent Sandbox controller, sycophant CRDs.

### 0.1 Disable Docker Desktop's bundled k8s

If it's currently enabled: Docker Desktop → Settings → Kubernetes → uncheck **Enable Kubernetes**. Wait for teardown.

### 0.2 Create the cluster

```sh
k3d cluster create sycophant-dev \
  --k3s-arg "--flannel-backend=none@server:*" \
  --k3s-arg "--disable-network-policy@server:*" \
  --k3s-arg "--disable=traefik@server:*" \
  --k3s-arg "--disable=servicelb@server:*" \
  -v "$HOME/sycophant/tmp:$HOME/sycophant/tmp@all" \
  --registry-create k3d-registry.localhost:0.0.0.0:5555 \
  --port "9090:9090@loadbalancer"
```

We keep k3s's bundled kube-proxy and run Cilium for CNI + CiliumNetworkPolicy enforcement only. Cilium's full kube-proxy replacement (socket-LB based ClusterIP routing) doesn't work cleanly on k3d's containerd-2.0 + cgroup-v2 environment in 1.19.3 — pods can't reach ClusterIPs. With kube-proxy retained, the full kpr complexity is avoided and ClusterIP routing works out of the box.

The `-v` mount uses the same absolute path on both host and node so the chart's hostPath references resolve transparently. The `--registry-create` provisions an in-cluster OCI registry at `k3d-registry.localhost:5555` for chamber images.

### 0.3 Install Cilium

```sh
K3D_API_HOST=$(docker inspect k3d-sycophant-dev-server-0 \
  -f '{{ range $k, $v := .NetworkSettings.Networks }}{{ $v.IPAddress }}{{ end }}')

helm repo add cilium https://helm.cilium.io/
helm repo update
helm install cilium cilium/cilium --version 1.19.3 \
  --namespace kube-system \
  --set k8sServiceHost="$K3D_API_HOST" \
  --set k8sServicePort=6443 \
  --set kubeProxyReplacement=false \
  --set cni.exclusive=false

kubectl wait -n kube-system --for=condition=Ready --timeout=180s \
  pod -l app.kubernetes.io/part-of=cilium
```

`cni.exclusive=false` is required on k3d to coexist with k3s's bundled CNI config dir. `kubeProxyReplacement=false` keeps k3s's bundled kube-proxy in charge of ClusterIP routing — Cilium handles CNI + network policy only.

### 0.4 Install gVisor (runsc) on the k3d node

```sh
K3D_NODE=k3d-sycophant-dev-server-0
ARCH=aarch64
URL=https://storage.googleapis.com/gvisor/releases/release/latest/${ARCH}

docker exec "$K3D_NODE" sh -c "
  cd /tmp && set -eu
  wget -q ${URL}/runsc ${URL}/runsc.sha512
  wget -q ${URL}/containerd-shim-runsc-v1 ${URL}/containerd-shim-runsc-v1.sha512
  sha512sum -c runsc.sha512 -c containerd-shim-runsc-v1.sha512
  chmod a+rx runsc containerd-shim-runsc-v1
  mv runsc containerd-shim-runsc-v1 /usr/local/bin/
"

docker exec "$K3D_NODE" sh -c 'cat > /var/lib/rancher/k3s/agent/etc/containerd/config-v3.toml.tmpl <<TMPL
{{ template "base" . }}

[plugins."io.containerd.cri.v1.runtime".containerd.runtimes.runsc]
  runtime_type = "io.containerd.runsc.v1"
TMPL'

docker exec "$K3D_NODE" sh -c 'kill -HUP $(pidof k3s)'
sleep 5

kubectl apply -f - <<EOF
apiVersion: node.k8s.io/v1
kind: RuntimeClass
metadata:
  name: gvisor
handler: runsc
EOF
```

### 0.5 Smoke test gVisor before deploying the chart

```sh
kubectl run gvisor-smoke --rm -i --restart=Never \
  --overrides='{"spec":{"runtimeClassName":"gvisor"}}' \
  --image=busybox:stable -- dmesg | head -3
```

Expected: a `Starting gVisor...` line. If absent, the containerd template is wrong; inspect `docker exec $K3D_NODE cat /var/lib/rancher/k3s/agent/etc/containerd/config.toml` for the rendered config.

### 0.6 Install Agent Sandbox v0.3.10

```sh
kubectl apply -f https://github.com/kubernetes-sigs/agent-sandbox/releases/download/v0.3.10/manifest.yaml
kubectl apply -f https://github.com/kubernetes-sigs/agent-sandbox/releases/download/v0.3.10/extensions.yaml
```

### Cluster recovery

`k3d cluster delete sycophant-dev` wipes everything including runsc binaries. To rebuild, re-run Step 0 from the top. `k3d cluster stop/start` preserves runsc + Cilium across Docker restarts.

## Step 1: Build images

Cross-compile all binaries and build Docker images locally.

```sh
cd ~/sycophant

# All Rust binaries
cargo build --release --target aarch64-unknown-linux-musl \
  -p tightbeam-controller -p tightbeam-llm-job \
  -p airlock-controller -p airlock-runtime \
  -p transponder -p mainframe-runtime -p mainframe-controller

# Scratch images for the components whose local tag matches the binary name
for bin in tightbeam-controller tightbeam-llm-job airlock-controller airlock-runtime mainframe-controller; do
  cp target/aarch64-unknown-linux-musl/release/$bin ${bin}-linux-musl-arm64
  docker build -f build/Dockerfile --build-arg BINARY=$bin --build-arg TARGETARCH=arm64 -t ${bin}:local .
  rm ${bin}-linux-musl-arm64
done

# Transponder image is published upstream as sycophant-transponder, so the
# local tag has to match — chart values reference sycophant-transponder:local.
cp target/aarch64-unknown-linux-musl/release/transponder transponder-linux-musl-arm64
docker build -f build/Dockerfile --build-arg BINARY=transponder --build-arg TARGETARCH=arm64 -t sycophant-transponder:local .
rm transponder-linux-musl-arm64

# Mainframe-runtime (alpine, needs git)
cp target/aarch64-unknown-linux-musl/release/mainframe-runtime /tmp/mainframe-runtime
echo 'FROM alpine:3.21
RUN apk add --no-cache git
COPY --chmod=755 mainframe-runtime /usr/local/bin/mainframe-runtime
ENTRYPOINT ["mainframe-runtime"]' > /tmp/Dockerfile.mainframe-runtime
docker build -f /tmp/Dockerfile.mainframe-runtime -t sycophant-mainframe-runtime:local /tmp/
rm /tmp/mainframe-runtime /tmp/Dockerfile.mainframe-runtime

# Chamber images (need airlock-runtime in build context)
cp target/aarch64-unknown-linux-musl/release/airlock-runtime images/git/airlock-runtime-linux-arm64
docker build --build-arg TARGETARCH=arm64 -f images/git/Dockerfile images/git/ -t airlock-git:local
rm images/git/airlock-runtime-linux-arm64

cp target/aarch64-unknown-linux-musl/release/airlock-runtime examples/scenarios/ssh-secret/airlock-runtime-linux-arm64
docker build --build-arg TARGETARCH=arm64 examples/scenarios/ssh-secret/ -t airlock-ssh:local
rm examples/scenarios/ssh-secret/airlock-runtime-linux-arm64
```

Load images into the k3d cluster:

```sh
for img in tightbeam-controller:local tightbeam-llm-job:local \
           airlock-controller:local mainframe-controller:local \
           sycophant-transponder:local sycophant-mainframe-runtime:local; do
  k3d image import "$img" --cluster sycophant-dev
done

# Pre-pull Versitygw so bundled-mode pods don't have to fetch from Docker Hub
docker pull versity/versitygw:v1.0.18
k3d image import versity/versitygw:v1.0.18 --cluster sycophant-dev
```

Push chamber images to the in-cluster registry that `k3d cluster create --registry-create` provisioned (airlock reads OCI labels via HTTP):

```sh
for img in airlock-git airlock-ssh; do
  docker tag ${img}:local k3d-registry.localhost:5555/${img}:latest
  docker push k3d-registry.localhost:5555/${img}:latest
done
```

## Step 2: Configure

### Namespace

Create up front so subsequent steps can reference it.

```sh
kubectl create namespace e2e-test --dry-run=client -o yaml | kubectl apply -f -
```

### Mainframe sources (per-workspace)

Stage 4 of decision 008: each workspace configures its own mainframe via
`workspaces.<name>.instructions:`. The e2e covers **local mode only** —
each workspace points at a hostPath under `~/sycophant/tmp/`, the chart
provisions a per-workspace Versitygw against that path, and
mainframe-controller pulls from each Versitygw into its own PVC subdir.
External-S3 wiring is covered by helm-template + Rust unit tests in CI,
not by the e2e.

Seed the per-workspace fixtures directly on your machine. The k3d cluster
created in Step 0.2 mounts `~/sycophant/tmp` at the same path inside the
node container, so the cluster sees changes live without any sync step.
The chart's bundled-mode mounts the `instructions/` subdirectory inside
each path as the S3 bucket exposed by Versitygw, so fixtures go under that
subdirectory:

```sh
# hello-world: simple AGENTS.md
mkdir -p ~/sycophant/tmp/hello-world-data/instructions
cp examples/mainframe/simple/AGENTS.md \
  ~/sycophant/tmp/hello-world-data/instructions/AGENTS.md

# multi-agent: orchestrator ENTRYPOINT + delegate persona files
mkdir -p ~/sycophant/tmp/multi-agent-data/instructions
cp -R examples/mainframe/orchestrator/. \
  ~/sycophant/tmp/multi-agent-data/instructions/
```

The chart will deploy two Versitygw Deployment+Service+Secret stacks (one
per workspace). mainframe-controller pulls from
`http://e2e-test-<workspace>-mainframe-s3.e2e-test.svc:7070/instructions/`
for each.

See [docs/mainframe.md](mainframe.md) for the full Mainframe layout.

### LLM secrets and chamber fixtures

```sh
# Default model (Mistral) needs its own secret. Anthropic models still
# used for haiku/sonnet alternates.
kubectl create secret generic sycophant-llm-mistral \
  --namespace e2e-test \
  --from-literal=api-key="$MISTRAL_API_KEY" \
  --dry-run=client -o yaml | kubectl apply -f -

kubectl create secret generic sycophant-llm-anthropic \
  --namespace e2e-test \
  --from-literal=api-key="$ANTHROPIC_API_KEY" \
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
kubectl get sandbox -n e2e-test
kubectl get pods -n e2e-test
kubectl get tightbeammodels -n e2e-test
kubectl get mainframes -n e2e-test
kubectl logs -n e2e-test hello-world -c transponder
kubectl logs -n e2e-test deployment/airlock-controller
kubectl logs -n e2e-test deployment/mainframe-controller

# Mainframe and conversation-log mounts — both workspaces should see their
# own AGENTS.md (different content per source).
kubectl exec -n e2e-test hello-world -c mainframe-runtime -- ls /etc/mainframe
kubectl exec -n e2e-test hello-world -c mainframe-runtime -- cat /etc/mainframe/AGENTS.md
kubectl exec -n e2e-test multi-agent -c mainframe-runtime -- cat /etc/mainframe/AGENTS.md
kubectl exec -n e2e-test hello-world -c mainframe-runtime -- ls /var/log/conversation

# Bundled Versitygw — one per workspace
kubectl get deployment -n e2e-test -l app.kubernetes.io/component=mainframe-bundled-s3
kubectl get svc        -n e2e-test -l app.kubernetes.io/component=mainframe-bundled-s3

# Mainframe controller PVC has per-workspace subdirs
MFPOD=$(kubectl get pod -n e2e-test \
  -l app.kubernetes.io/name=mainframe-controller -o name | head -1 | sed 's|pod/||')
kubectl exec -n e2e-test "$MFPOD" -- ls /data/mainframe
```

Expected:
- Sandbox CRs `hello-world` and `multi-agent` exist (workspaces run as
  agent-sandbox Sandbox CRs with gVisor kernel isolation)
- All pods running (workspace pods show 2/2: transponder + mainframe-runtime)
- Models registered (`kubectl get tightbeammodels` shows `default` plus
  any anthropic.* alternates)
- Two Mainframe CRs (`hello-world`, `multi-agent`), both `Ready=True`
- Two bundled Versitygw Deployments (`e2e-test-hello-world-mainframe-s3`
  and `e2e-test-multi-agent-mainframe-s3`) exist and are reachable from
  mainframe-controller
- mainframe-controller's `/data/mainframe` directory contains two subdirs
  named `hello-world` and `multi-agent`, each with content from its respective hostPath
- Transponder: `connected to tightbeam controller`, `connected to airlock
  controller`, `loaded entrypoint, path=/etc/mainframe/AGENTS.md, bytes=N`,
  `tool router initialized, count=N`, `subscribed to tightbeam for inbound messages`.
- Airlock: `discovered tools from image`, `chamber watcher initial sync complete, tool_count=N`
- Mainframe-controller: `synced from s3, object_count=N, revision=...` (one log line per CR)
- Each workspace's `/etc/mainframe/AGENTS.md` reflects the fixture
  copied into its respective hostPath
- The conversation-log mount lists `<workspace>` subdirectories (writes are blocked; read-only mount)

### Verify edit + delete propagation

The trust contract is that the principal's source is authoritative — adds, edits, and deletes all converge at the workspace pod within one `refreshIntervalSeconds` tick.

```sh
# Edit propagation: append a marker to the source, wait for the next tick,
# confirm it shows up in the workspace pod's mount.
echo "" >> ~/sycophant/tmp/hello-world-data/instructions/AGENTS.md
echo "<!-- LIVE EDIT $(date +%s) -->" >> ~/sycophant/tmp/hello-world-data/instructions/AGENTS.md
sleep 35
kubectl exec -n e2e-test hello-world -c mainframe-runtime -- \
  grep "LIVE EDIT" /etc/mainframe/AGENTS.md
# Expected: matches the marker.

# Delete propagation: add a temp file, confirm it appears, remove it, confirm
# it disappears.
echo "scratch" > ~/sycophant/tmp/hello-world-data/instructions/temp.md
sleep 35
kubectl exec -n e2e-test hello-world -c mainframe-runtime -- \
  test -f /etc/mainframe/temp.md && echo "added: PASS"

rm ~/sycophant/tmp/hello-world-data/instructions/temp.md
sleep 35
kubectl exec -n e2e-test hello-world -c mainframe-runtime -- \
  test ! -f /etc/mainframe/temp.md && echo "deleted: PASS"

# Restore the live-edit marker
sed -i '' '/LIVE EDIT/d; /^$/d' ~/sycophant/tmp/hello-world-data/instructions/AGENTS.md
```

Both should pass. If "deleted: PASS" doesn't print, the controller's `--delete-after` orphan walk regressed — check `kubectl logs deployment/mainframe-controller` for "synced from s3" lines and inspect `/data/mainframe/hello-world/` for stale files via a debug pod.

## Step 5: Chat

```sh
kubectl port-forward -n e2e-test svc/tightbeam-controller 9090:9090 &
sleep 2

grpcurl -plaintext -max-time 60 -d '{"register":{"channel_type":"test","channel_name":"e2e","workspace":"hello-world"}}
{"user_message":{"content":[{"text":{"text":"Use the ssh tool to run: cat /home/agent/.ssh/id_ed25519"}}],"sender":"tester"}}' \
  localhost:9090 tightbeam.v1.TightbeamController/ChannelStream

kill %1
```

Expected: JSON response with `sendMessage.content[].text` containing
the LLM's reply. The response arrives after 10-30 seconds (cold LLM
Job startup + API call + tool execution). The LLM should call the ssh
tool. The chamber has a demo SSH key staged to `/home/agent/.ssh/id_ed25519`.

### Verify the entrypoint path actually fired

The transponder logs the entrypoint load at startup and the user turn dispatch on each inbound message:

```sh
kubectl logs -n e2e-test hello-world -c transponder | \
  grep -E "loaded entrypoint|received inbound message|tool router initialized"
```

Expected: one `loaded entrypoint, path=/etc/mainframe/AGENTS.md, bytes=N` line at startup, plus one `received inbound message` line per `grpcurl` send.

### Inspect the conversation log for audit/replay

In entrypoint mode the conversation log captures each user turn and the agent's reply. When the orchestrator pattern uses `llm_call`, the delegate's call is also persisted with `tag: delegate`:

```sh
TBPOD=$(kubectl get pod -n e2e-test \
  -l app.kubernetes.io/name=tightbeam-controller -o name | head -1 | sed 's|pod/||')
kubectl debug -n e2e-test "$TBPOD" --image=busybox:1.36 \
  --target=controller --profile=general -it=false -- \
  cat /proc/1/root/var/log/tightbeam/hello-world/conversation.ndjson
```

**Simple AGENTS.md** — expected two entries per user turn:
1. `{"role":"user","content":[{"type":"text","text":"..."}]}` — the user's input.
2. `{"role":"assistant","content":[{"type":"text","text":"..."}]}` — the agent's reply. No `tag` field.

**Orchestrator AGENTS.md** — when the LLM uses `llm_call`, the conversation log should contain:
- Untagged main-thread entries: user input, orchestrator's `tool_use` of `llm_call`, the eventual `tool_result`, and the orchestrator's final reply.
- At least one delegate-tagged pair: `{"role":"user",...,"tag":"delegate"}` (the delegate's `query` argument) followed by `{"role":"assistant",...,"tag":"delegate"}` (the delegate's response).

Quick filter to confirm the tag fires:

```sh
kubectl debug -n e2e-test "$TBPOD" --image=busybox:1.36 \
  --target=controller --profile=general -it=false -- \
  grep '"tag":"delegate"' /proc/1/root/var/log/tightbeam/hello-world/conversation.ndjson | wc -l
```

Expected: ≥ 2 lines per orchestrator turn that delegated (one user, one assistant).

## Step 6: Verify security

### gVisor kernel isolation

```sh
kubectl exec -n e2e-test hello-world -c mainframe-runtime -- dmesg | head -1
```

Expected: `Starting gVisor...` — confirms the workspace runs under
gVisor's sandboxed kernel, not the host kernel.

### Secret scrubbing

```sh
kubectl logs -n e2e-test hello-world -c transponder | grep -c "FAKE-ED25519-PRIVATE-KEY"
```

Expected: 0. The scrubber replaces it with `[REDACTED:demo-ssh-key]`.

### Tool execution

```sh
kubectl logs -n e2e-test deployment/airlock-controller | grep "received tool result"
```

Expected: `received tool result, call_id=..., exit_code=0`

### NetworkPolicy enforcement

```sh
kubectl exec -n e2e-test hello-world -c mainframe-runtime -- \
  wget -qO- --timeout=3 https://httpbin.org/ip 2>&1
```

Expected: timeout. Workspace has no internet access.

### Credential isolation

```sh
kubectl exec -n e2e-test hello-world -c mainframe-runtime -- \
  cat /run/secrets/llm/api-key 2>&1
```

Expected: "No such file or directory". No secrets mounted in workspace.

### Workspace scoping

```sh
kubectl get serviceaccounts -n e2e-test -l sycophant.io/type=workspace-sa
kubectl exec -n e2e-test hello-world -c transponder -- \
  ls /var/run/secrets/kubernetes.io/serviceaccount/token
kubectl logs -n e2e-test deployment/airlock-controller | grep "workspace bindings"
```

Expected:
- ServiceAccounts `sa-hello-world` and `sa-multi-agent` exist
- SA token file is mounted in the transponder container
- Controller log shows `loaded workspace bindings`

## Step 7: Teardown

To remove just the chart (keep the cluster):

```sh
helm uninstall e2e-test --namespace e2e-test
kubectl delete namespace e2e-test
```

To wipe the whole cluster (also removes runsc + Cilium — full recreate requires Step 0):

```sh
k3d cluster delete sycophant-dev
```

Verify chart removal:
```sh
helm status e2e-test -n e2e-test
```

Expected: `Error: release: not found`

## Troubleshooting

### Transponder CrashLoopBackOff
```sh
kubectl logs -n e2e-test hello-world -c transponder --previous
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

### Stale image cache after rebuild
Containerd caches images by `name:tag`, not by content. After
`docker build -t foo:local .` and a re-import, running pods may keep
using the OLD image (visible by mismatched `imageID` in
`kubectl describe pod` vs the freshly-built image's `docker images foo:local`).
Force the cache to drop with `--replace`:

```sh
k3d image import <image>:local --cluster sycophant-dev --replace
kubectl delete pod -n e2e-test <pod-using-the-image>
```

### Sandbox CR stuck after pod deletion
The agent-sandbox controller stores the workspace pod's name in an
annotation. If the pod is deleted (e.g., to apply a new transponder
image) without the Sandbox CR being aware, the controller may loop
on `Pod "<name>" not found` and refuse to recreate the pod.

Recovery: dump the spec, strip stale metadata, delete the CR, reapply.

```sh
kubectl get sandbox -n e2e-test <name> -o yaml > /tmp/sb.yaml
python3 -c "
import yaml
data = yaml.safe_load(open('/tmp/sb.yaml'))
data.pop('status', None)
for k in ['annotations', 'managedFields', 'resourceVersion', 'uid',
          'creationTimestamp', 'generation', 'finalizers']:
    data['metadata'].pop(k, None)
print(yaml.safe_dump(data))
" > /tmp/sb-clean.yaml
kubectl delete sandbox -n e2e-test <name>
kubectl apply -f /tmp/sb-clean.yaml
```

### Wipe conversation logs between runs
Tightbeam persists conversation history to `/var/log/tightbeam/<workspace>/`.
Stale entries from a previous run (especially failed turns or different
schema-mode behavior) can mislead the LLM on subsequent turns. Wipe
before re-testing:

```sh
TBPOD=$(kubectl get pod -n e2e-test \
  -l app.kubernetes.io/name=tightbeam-controller -o name | head -1 | sed 's|pod/||')
kubectl debug -n e2e-test "$TBPOD" --image=busybox:1.36 \
  --target=controller --profile=general -it=false -- \
  rm -rf /proc/1/root/var/log/tightbeam/hello-world \
         /proc/1/root/var/log/tightbeam/multi-agent
kubectl rollout restart deployment tightbeam-controller -n e2e-test
```


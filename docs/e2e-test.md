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

The cluster runs on k3d (k3s in Docker). This is the supported runtime for sycophant local self-host because the workspace pod's `/etc/mainframe` is a kubelet `hostPath` mount, which requires the cluster node to see your host filesystem. Docker Desktop's bundled k8s does not expose `/Users` to its kind node, so it doesn't support the HostPath workflow out of the box.

## Step 0: Bootstrap k3d cluster

A clean cluster bootstrap covers: k3d cluster create, Cilium CNI, gVisor runtime, Agent Sandbox controller. Sycophant CRDs arrive in Step 3 via `helm install`.

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
  --registry-create sycophant-registry:0.0.0.0:5555 \
  --port "9090:9090@loadbalancer"
```

We keep k3s's bundled kube-proxy and run Cilium for CNI + CiliumNetworkPolicy enforcement only. Cilium's full kube-proxy replacement (socket-LB based ClusterIP routing) doesn't work cleanly on k3d's containerd-2.0 + cgroup-v2 environment in 1.19.3 — pods can't reach ClusterIPs. With kube-proxy retained, the full kpr complexity is avoided and ClusterIP routing works out of the box.

The `-v` mount uses the same absolute path on both host and node so the chart's hostPath references resolve transparently. The `--registry-create` provisions an in-cluster OCI registry. The hostname `sycophant-registry` (no `.localhost` TLD) avoids RFC 6761's libc loopback-bypass — musl-linked Rust controllers resolve it via CoreDNS like any other in-cluster name. From the host, the same registry is reachable at `localhost:5555` (host:5555 → container:5000 via Docker port mapping; the in-cluster reference is `sycophant-registry:5000`).

Registry config is bootstrap-only: do not edit `/etc/rancher/k3s/registries.yaml` on a running k3s node. K3s embeds containerd as a subprocess; reloading via `kill -HUP $(pidof k3s)` restarts both, brown-outs the CRI socket, and crashes any running CNI agents (Cilium). For runtime additions, write `/etc/containerd/certs.d/<host>/hosts.toml` instead — containerd reloads it on the next pull without daemon restart.

### 0.3 Install gVisor (runsc) on the k3d node

gVisor must be installed *before* Cilium. The runsc setup writes a containerd template and HUP's k3s to reload it; k3s embeds containerd as a subprocess, so the HUP restarts both. If Cilium is already installed when this happens, its agent's CRI socket disappears mid-restart and the daemonset enters CrashLoopBackOff. By installing gVisor first, the HUP fires when no DaemonSets depend on the CRI yet — no cascade.

```sh
K3D_NODE=k3d-sycophant-dev-server-0
ARCH=aarch64
URL=https://storage.googleapis.com/gvisor/releases/release/latest/${ARCH}

# Download on the host (k3d's node image only ships busybox wget, which
# doesn't speak HTTPS), then docker cp into the node.
cd /tmp
curl -sSfL -o runsc                      ${URL}/runsc
curl -sSfL -o runsc.sha512               ${URL}/runsc.sha512
curl -sSfL -o containerd-shim-runsc-v1   ${URL}/containerd-shim-runsc-v1
curl -sSfL -o containerd-shim-runsc-v1.sha512 ${URL}/containerd-shim-runsc-v1.sha512
sha512sum -c runsc.sha512 -c containerd-shim-runsc-v1.sha512
chmod +x runsc containerd-shim-runsc-v1

docker exec "$K3D_NODE" mkdir -p /usr/local/bin
docker cp runsc                    "$K3D_NODE":/usr/local/bin/runsc
docker cp containerd-shim-runsc-v1 "$K3D_NODE":/usr/local/bin/containerd-shim-runsc-v1
rm -f runsc runsc.sha512 containerd-shim-runsc-v1 containerd-shim-runsc-v1.sha512

docker exec "$K3D_NODE" sh -c 'cat > /var/lib/rancher/k3s/agent/etc/containerd/config-v3.toml.tmpl <<TMPL
{{ template "base" . }}

[plugins."io.containerd.cri.v1.runtime".containerd.runtimes.runsc]
  runtime_type = "io.containerd.runsc.v1"
TMPL'

docker exec "$K3D_NODE" sh -c 'kill -HUP $(pidof k3s)'
# Poll for API health (not node Ready — node stays NotReady until Cilium is installed in 0.4).
until kubectl get --raw /healthz 2>/dev/null | grep -q '^ok$'; do sleep 2; done

kubectl apply -f - <<EOF
apiVersion: node.k8s.io/v1
kind: RuntimeClass
metadata:
  name: gvisor
handler: runsc
EOF
```

### 0.4 Install Cilium

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
  pod -l app.kubernetes.io/part-of=cilium,app.kubernetes.io/name=cilium-agent
```

`cni.exclusive=false` is required on k3d to coexist with k3s's bundled CNI config dir. `kubeProxyReplacement=false` keeps k3s's bundled kube-proxy in charge of ClusterIP routing — Cilium handles CNI + network policy only. The wait selector targets only the cilium-agent (the second cilium-operator replica stays Pending on a single-node cluster due to a hostPort conflict — leader handles everything).

### 0.5 Smoke test gVisor before deploying the chart

```sh
kubectl run gvisor-smoke --rm -i --restart=Never \
  --overrides='{"spec":{"runtimeClassName":"gvisor"}}' \
  --image=busybox:stable -- dmesg | head -3
```

Expected: a `Starting gVisor...` line. If absent, the containerd template is wrong; inspect `docker exec $K3D_NODE cat /var/lib/rancher/k3s/agent/etc/containerd/config.toml` for the rendered config.

### 0.6 Install Agent Sandbox v0.4.5

```sh
kubectl apply -f https://github.com/kubernetes-sigs/agent-sandbox/releases/download/v0.4.5/manifest.yaml
kubectl apply -f https://github.com/kubernetes-sigs/agent-sandbox/releases/download/v0.4.5/extensions.yaml
```

v0.4.5 fixes a v0.3.10 regression where the Sandbox controller refused to recreate a workspace pod after the pod was deleted out-of-band ([upstream issue #611](https://github.com/kubernetes-sigs/agent-sandbox/issues/611), fixed in v0.4.2 via [PR #613](https://github.com/kubernetes-sigs/agent-sandbox/pull/613)). The CRD still serves `agents.x-k8s.io/v1alpha1`; no chart changes required for the bump.

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
```

Push chamber images to the in-cluster registry that `k3d cluster create --registry-create` provisioned (airlock reads OCI labels via HTTP):

```sh
for img in airlock-git airlock-ssh; do
  docker tag ${img}:local localhost:5555/${img}:latest
  docker push localhost:5555/${img}:latest
done
```

## Step 2: Configure

### Namespace

Create up front so subsequent steps can reference it.

```sh
kubectl create namespace e2e-test --dry-run=client -o yaml | kubectl apply -f -
```

### Mainframe sources (per-workspace)

Per ADR 010, each workspace configures its own mainframe via
`workspaces.<name>.instructions:` — an absolute host filesystem path. The
chart renders a Mainframe CR with `spec.source.kind: HostPath` and a
Sandbox whose pod mounts that directory read-only at `/etc/mainframe` via
a `hostPath` volume. v0 ships HostPath only; non-HostPath source kinds
ship as separate-repo adapters.

Seed the per-workspace fixtures directly on your machine. The k3d cluster
created in Step 0.2 mounts `~/sycophant/tmp` at the same path inside the
node container, so the cluster sees changes live without any sync step.
Fixtures go directly into the workspace's `instructions:` path (no
intermediate `instructions/` subdirectory — that was a Versitygw bucket
layout, no longer present):

```sh
# hello-world: simple AGENTS.md
mkdir -p ~/sycophant/tmp/hello-world-data
cp examples/mainframe/simple/AGENTS.md \
  ~/sycophant/tmp/hello-world-data/AGENTS.md

# multi-agent: orchestrator AGENTS.md + delegate persona files
mkdir -p ~/sycophant/tmp/multi-agent-data
cp -R examples/mainframe/orchestrator/. \
  ~/sycophant/tmp/multi-agent-data/
```

The chart renders one Mainframe CR per workspace. The workspace pod's
`/etc/mainframe` mount is the live host directory — edits land in the
pod's view on the next read; mainframe-controller does no fetch.

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
```

Expected:
- Sandbox CRs `hello-world` and `multi-agent` exist (workspaces run as
  agent-sandbox Sandbox CRs with gVisor kernel isolation)
- All pods running (workspace pods show 2/2: transponder + mainframe-runtime)
- Models registered (`kubectl get tightbeammodels` shows `default` plus
  any anthropic.* alternates)
- Two Mainframe CRs (`hello-world`, `multi-agent`) with `kind: HostPath`
- Transponder: `connected to tightbeam controller`, `connected to airlock
  controller`, `loaded entrypoint, path=/etc/mainframe/AGENTS.md, bytes=N`,
  `tool router initialized, count=N`, `subscribed to tightbeam for inbound messages`.
- Airlock: `discovered tools from image`, `chamber watcher initial sync complete, tool_count=N`
- Mainframe-controller: `mainframe watcher initial sync complete, mainframe_count=2`,
  `reconcile no-op for HostPath` per CR (v0 reconciliation is a no-op)
- Each workspace's `/etc/mainframe/AGENTS.md` reflects the fixture
  copied into its respective hostPath
- The conversation-log mount lists `<workspace>` subdirectories (writes are blocked; read-only mount)

### Verify edit + delete propagation

The workspace pod's `/etc/mainframe` is a kubelet `hostPath` mount of the host
directory. Edits in the source land in the pod's view on the next file read —
no sync interval, no fetch. Deletes propagate just as immediately.

```sh
# Edit propagation: append a marker to the source, confirm it's visible inside
# the pod immediately.
echo "" >> ~/sycophant/tmp/hello-world-data/AGENTS.md
echo "<!-- LIVE EDIT $(date +%s) -->" >> ~/sycophant/tmp/hello-world-data/AGENTS.md
kubectl exec -n e2e-test hello-world -c mainframe-runtime -- \
  grep "LIVE EDIT" /etc/mainframe/AGENTS.md
# Expected: matches the marker.

# Delete propagation: add a temp file, confirm it appears, remove it, confirm
# it disappears.
echo "scratch" > ~/sycophant/tmp/hello-world-data/temp.md
kubectl exec -n e2e-test hello-world -c mainframe-runtime -- \
  test -f /etc/mainframe/temp.md && echo "added: PASS"

rm ~/sycophant/tmp/hello-world-data/temp.md
kubectl exec -n e2e-test hello-world -c mainframe-runtime -- \
  test ! -f /etc/mainframe/temp.md && echo "deleted: PASS"

# Restore the live-edit marker
sed -i '' '/LIVE EDIT/d; /^$/d' ~/sycophant/tmp/hello-world-data/AGENTS.md
```

Both should pass. If propagation lags, kubelet hostPath caching or k3d bind-mount staleness is the likely cause; inspect the host directory directly to confirm the source state.

The agent's effective system prompt is re-read on every turn from
`/etc/mainframe/AGENTS.md`, so principal edits to the host directory
take effect on the agent's next message — no pod restart needed. The
file-level edit/delete tests above verify the kubelet mount; running
the chat path (Step 5) after an edit verifies the agent picks up the
new prompt.

### Verify dynamic chamber-tool refresh

Chamber tool changes propagate to running workspaces via airlock-controller's
`WatchTools` server-streaming RPC; the transponder applies pushed snapshots
in a background task without a pod restart.

```sh
RESTART_BEFORE=$(kubectl get pod -n e2e-test hello-world \
  -o jsonpath='{.status.containerStatuses[?(@.name=="transponder")].restartCount}')

# Trigger a chamber re-discovery by re-applying the AirlockChamber CR
# (annotation bump forces the watcher to fire Apply, which re-runs tool
# discovery and bumps the tools_revision counter).
kubectl annotate airlockchamber -n e2e-test ssh-secret \
  e2e/refresh="$(date +%s)" --overwrite

# Assert the refresh log appears in the transponder within 30s.
# (kube-rs watcher latency on annotation-only Apply events can be ~15s.)
END=$(($(date +%s) + 30))
until kubectl logs -n e2e-test hello-world -c transponder 2>/dev/null \
  | grep -q "tool router refreshed"; do
  [ "$(date +%s)" -ge "$END" ] && { echo "refresh log: FAIL (timeout)"; break; }
  sleep 1
done
kubectl logs -n e2e-test hello-world -c transponder 2>&1 \
  | grep "tool router refreshed" | tail -1

# Assert the workspace pod did NOT restart.
RESTART_AFTER=$(kubectl get pod -n e2e-test hello-world \
  -o jsonpath='{.status.containerStatuses[?(@.name=="transponder")].restartCount}')
[ "$RESTART_BEFORE" = "$RESTART_AFTER" ] \
  && echo "no restart: PASS" \
  || echo "no restart: FAIL (before=$RESTART_BEFORE after=$RESTART_AFTER)"
```

Expected: at least one `tool router refreshed count=N` log line in the
transponder, and `restartCount` unchanged. If the log is absent, the
background `WatchTools` task isn't running — check transponder logs for
`watch_tools subscribe failed` or `watch_tools stream error`.

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
  Check RBAC for `sycophant.md/airlockchambers` watch permission.

### Conversation corruption (API error 400: tool_use without tool_result)
Rare since chamber-tool refresh no longer requires pod restarts (chamber updates propagate via `WatchTools`; bindings updates propagate via Helm checksum on chart upgrade). Can still surface if a tool call is mid-flight when the transponder crashes — orphaned `tool_use` blocks in the conversation log break subsequent turns:
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
Containerd caches images by `name:tag`, not by content. After `docker build -t foo:local .` and a re-import, running pods may keep using the OLD image (visible by mismatched `imageID` in `kubectl describe pod` vs the freshly-built image's `docker images foo:local`). k3d v5.8.3 doesn't have a `--replace`-style flag, so drop the image from the node's containerd store directly before re-importing:

```sh
docker exec k3d-sycophant-dev-server-0 \
  ctr -n k8s.io image rm docker.io/library/<image>:local
k3d image import <image>:local --cluster sycophant-dev
kubectl rollout restart deployment/<deploy-using-the-image> -n e2e-test
```

For workspace pod refresh, scale the Sandbox CR down and back up — never `kubectl delete pod` directly:

```sh
kubectl patch sandbox -n e2e-test hello-world --type=merge -p '{"spec":{"replicas":0}}'
kubectl wait --for=delete pod -n e2e-test -l agents.x-k8s.io/sandbox-name=hello-world --timeout=60s
kubectl patch sandbox -n e2e-test hello-world --type=merge -p '{"spec":{"replicas":1}}'
```

Note: workspace pod refresh is rarely needed in normal ops. Chamber tool changes propagate via the dynamic-refresh path (Step 4) without restart; operator-driven binding changes propagate via `helm upgrade` (the airlock-controller deployment has `checksum/bindings` and `checksum/scheduling` annotations that change with the ConfigMaps, triggering a rolling restart automatically).

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


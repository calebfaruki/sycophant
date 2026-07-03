# End-to-End Test Guide

Stand up sycophant from nothing and assert the security clauses hold — driven by the `syco` CLI. Workspaces run as Pods with gVisor kernel isolation.

The e2e is the CLI plus a scenario runbook: `syco setup` brings up the cluster and builds the images; a scenario (e.g. [hello-world](../examples/scenarios/hello-world/README.md)) wires content and exercises the workspace from the Flutter client; `syco tenant audit` asserts the security clauses. This doc covers the prereqs, what each phase lays down, the architecture rationale behind those steps, and how to debug when something breaks.

> [`scripts/e2e.sh`](../scripts/e2e.sh) is the legacy maintainer harness — it also drives the Flutter client + Android emulator, which the CLI does not. It's retained until the CLI bring-up (`syco setup`'s image build + `syco tenant audit`) is verified on a live cluster, then retired.

## Prerequisites

`syco setup` checks these and prints a fix line for any that are missing, so you don't pre-verify by hand — but for reference:

- Docker running; `k3d` v5.8.3+, `kubectl`, `helm`, `grpcurl` on PATH
- From a checkout (the pre-1.0 path, where `syco setup` builds the images): the Rust + musl cross-build chain — the `<arch>-unknown-linux-musl` target, the `<arch>-linux-musl-gcc` cross-linker and its `~/.cargo/config.toml` line, protoc, cmake
- `OPENROUTER_API_KEY` in the environment

The cluster runs on k3d (k3s in Docker). This is the supported runtime for sycophant local self-host because a `HostPath` kernel is delivered through a cluster-scoped PV whose `hostPath` requires the cluster node to see your host filesystem (mounted into `mainframe-ctrl`, which serves it to the transponder over the `GetAgent` RPC). Docker Desktop's bundled k8s does not expose `/Users` to its kind node, so it doesn't support the HostPath workflow out of the box.

## Running the e2e

Follow a scenario runbook end to end:

- [hello-world](../examples/scenarios/hello-world/README.md) — the reference run: `setup` → `tenant up` → exercise (Flutter client) → `audit`.
- [ssh-credentials](../examples/scenarios/ssh-credentials/README.md) — the secret-scrubber fixture.

Each is the same shape:

```sh
syco setup                                            # cluster + images (idempotent)
# … wire content + workspace per the scenario …
syco tenant up    --ns <scenario>
# … from the Flutter client, send a tool-calling message (see the scenario) …
syco tenant audit <workspace> --ns <scenario>         # 7-check pass/fail (the threat model's six security clauses + the workspace-SA provisioning probe)
```

## What each phase lays down

| Phase | Command | What it lays down |
|---|---|---|
| Cluster | `syco setup` | k3d cluster → gVisor (runsc) → Cilium → CoreDNS registry wiring → Kyverno → sycophant cluster layer |
| Images | `syco setup` (from a checkout) | Cross-compile Rust → Docker build all images → `k3d image import` + push chambers to the in-cluster registry |
| Content | `syco tenant secret/provider/model/chamber set` | LLM creds, provider, model, chambers (CRs applied from outside the tenant) |
| Deploy | `syco tenant up` | Namespace labelled `part-of=sycophant-tenant` (Kyverno then mints the per-tenant TokenReview CRBs + pod VAP binding) → tenant chart |
| Exercise | Flutter client | A tool-calling message (sent from an enrolled client) lazy-spawns the stdlib chamber pod the audit probes |
| Audit | `syco tenant audit` | gVisor `dmesg`, secret-scrubbing count, airlock `exit_code=0`, egress timeout, L7 DNS block, no LLM creds in the sandbox, workspace SA |

## Architecture notes

**Why the preflight + disk budget.** `syco setup`'s `check_prereqs` runs first and fails fast with one fix line per missing prerequisite (macOS + Linux): always docker/k3d/helm/grpcurl, and — when building from a checkout — the musl cross-build chain (the rustup target, `<arch>-linux-musl-gcc`, and the `~/.cargo/config.toml` linker line, since the build links against the host `cc` and fails without it) plus protoc/cmake. It also gates Docker-VM free disk: **the VM is the constraint, not RAM.** A near-full VM trips kubelet imagefs eviction (~85%) and a Kyverno eviction storm that reads like a memory problem but isn't — keep ≳15 GB free (`setup` fails under 8 GB). The supported host-musl → `FROM scratch` build keeps the heavy build cache on the host, not the VM.

**Why `FLUTTER_TARGET=none` (backend-only).** Step 5 normally launches a local Flutter client; `none` skips that (and its toolchain in the preflight), deploys + runs the Step 6 security checks, and prints the connect details (server address, workspace, enrollment code) so a client can attach from another machine over Tailscale. This is the path for a headless host like a Mac mini with no Xcode.

**Why gVisor before Cilium.** The gVisor installer writes a containerd template and `HUP`s k3s to reload it. K3s embeds containerd as a subprocess, so the HUP restarts both. If Cilium is installed first, its agent's CRI socket disappears mid-restart and the DaemonSet enters CrashLoopBackOff. Installing gVisor first means the HUP fires when no DaemonSets depend on the CRI yet.

**Why kube-proxy stays + Cilium does CNI-only.** Cilium's full kube-proxy replacement (socket-LB based ClusterIP routing) doesn't work cleanly on k3d's containerd-2.0 + cgroup-v2 environment in 1.19.3 — pods can't reach ClusterIPs. With k3s's bundled kube-proxy retained, ClusterIP routing works out of the box. Cilium handles CNI + CiliumNetworkPolicy enforcement only.

**Why Kyverno is mandatory.** The cluster chart ships 3 ClusterPolicies + a ValidatingAdmissionPolicy. Without Kyverno's admission + background controllers, the policies install but never enforce. The mismatch only surfaces when downstream calls fail (e.g., airlock-ctrl SA tries `TokenReview` and the per-tenant ClusterRoleBinding the generator should have created doesn't exist).

**Why labelling the namespace provisions it.** The cluster chart's `tenant-rolebinding-generator` Kyverno policy matches any namespace carrying `app.kubernetes.io/part-of=sycophant-tenant` — name-independent, no deployer-SA requirement. Step 3 labels the `e2e-test` namespace after the cluster chart installs, and Kyverno then mints the three per-tenant TokenReview ClusterRoleBindings + the pod ValidatingAdmissionPolicyBinding. Generation is asynchronous, so Step 3 waits for the wiring before continuing, then applies a deliberately VAP-violating pod to assert the binding actually enforces.

**Why the registry hostname has no TLD.** k3d's `--registry-create sycophant-registry:0.0.0.0:5555` provisions an in-cluster OCI registry. The hostname `sycophant-registry` (no `.localhost` TLD) avoids RFC 6761's libc loopback-bypass — musl-linked Rust controllers resolve it via CoreDNS like any other in-cluster name. From the host, the same registry is reachable at `localhost:5555`.

**Why `--port "9090:9090@loadbalancer"`.** The cluster's serverlb maps host:9090 → cluster Service `tightbeam-ctrl:9090` (the internal listener). That's enough for the Layer 1 chat sanity. The external listener (`:9091`) is bound to 127.0.0.1 inside the controller pod and is reached via a separate `kubectl port-forward` (Step 5) so the emulator can hit it at `10.0.2.2:9091`.

**Why the Flutter app uses `10.0.2.2:9091`.** Android emulators map `10.0.2.2` to the host's loopback. The host port-forward exposes the controller's external listener there. No Tailscale/tsnet involvement on the device — same auth wire format (P-256 envelope-signed) as the phone-on-cellular path, just over loopback. `client/android/app/src/main/res/xml/network_security_config.xml` allows cleartext to that IP for h2c.

## Deferred — Layer 3 phone-on-cellular

Reaching the controller from a physical phone over cellular requires `headscale.enabled=true`, the tsnet-bridge sidecar, ACME-on-headscale binding the controller pod's :80/:443, a privileged `sudo kubectl port-forward 80:80 443:443` on the operator's laptop, the operator's router port-forwarding :80/:443 inbound, and a DNS A record pointing at the operator's public IP. Tailnet membership for the phone is provided by Tailscale Android pointed at the same headscale.

The full Layer-3 path is operator-network-specific and not in the script. Adding it behind a flag (`LAYER=3 ./scripts/e2e.sh`) is future work.

## Troubleshooting

### Transponder CrashLoopBackOff
```sh
kubectl logs -n e2e-test hello-world -c transponder --previous
```
- "subscribe stream closed": Controller restarted. Transponder will reconnect on next restart.
- "transport error" retries then fails: Controller unreachable. Check `kubectl get svc -n e2e-test` and `kubectl get endpoints -n e2e-test`.

### Airlock controller not ready
```sh
kubectl logs -n e2e-test deployment/airlock-ctrl
```
- "no k8s client available": ServiceAccount or RBAC misconfigured. Check `kubectl get sa -n e2e-test` and ClusterRoleBinding.
- "watcher kube client failed": Can't connect to Kubernetes API. Check RBAC for `sycophant.md/chambers` watch permission.

### Conversation corruption (API error 400: tool_use without tool_result)
Rare since chamber-tool refresh no longer requires pod restarts. Can still surface if a tool call is mid-flight when the transponder crashes — orphaned `tool_use` blocks in the conversation log break subsequent turns:
```sh
kubectl delete pvc --all -n e2e-test
kubectl rollout restart deployment hello-world -n e2e-test
```

### Turn stuck (no response after "received inbound message")
Check controller trace:
```sh
kubectl logs -n e2e-test deployment/hangar-ctrl
```
- No `turn: entry`: Transponder didn't send the Turn. Check transponder logs for errors.
- `enqueue_turn: complete` but no `wait_for_turn: recv complete`: No LLM Job connected. Check `kubectl get jobs -n e2e-test` and Job logs.
- `get_turn: received assignment` but no `stream_turn_result`: LLM Job got the assignment but the API call is slow or failing. Check Job logs.

### Stale image cache after rebuild
Containerd caches images by `name:tag`, not by content. After `docker build -t foo:local .` and a re-import, running pods may keep using the OLD image (visible by mismatched `imageID` in `kubectl describe pod` vs the freshly-built `docker images foo:local`). k3d v5.8.3 doesn't have a `--replace`-style flag, so drop the image from the node's containerd store before re-importing:

```sh
docker exec k3d-sycophant-dev-server-0 \
  ctr -n k8s.io image rm docker.io/library/<image>:local
k3d image import <image>:local --cluster sycophant-dev
kubectl rollout restart deployment/<deploy-using-the-image> -n e2e-test
```

For transponder pod refresh, restart the Deployment:

```sh
kubectl rollout restart -n e2e-test deployment/hello-world
kubectl rollout status -n e2e-test deployment/hello-world --timeout=60s
```

Note: transponder pod refresh is rarely needed in normal ops. Chamber tool changes propagate via the dynamic-refresh path without restart; operator-driven binding changes propagate via `helm upgrade` (the airlock-controller deployment has `checksum/bindings` and `checksum/scheduling` annotations that change with the ConfigMaps, triggering a rolling restart automatically).

### Wipe conversation logs between runs
The transponder persists conversation history to its own `<workspace>-conversation-data` PVC (mounted at `/var/lib/transponder/conversations`). Stale entries from a previous run can mislead the LLM on subsequent turns. Delete the PVC and restart the transponder so it starts from an empty log:

```sh
kubectl delete pvc hello-world-conversation-data -n e2e-test
kubectl rollout restart deployment hello-world -n e2e-test
```

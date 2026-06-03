# DevOps End-to-End Test Guide

Test the sycophant Helm chart against locally built images. Workspaces run as Pods with gVisor kernel isolation.

The procedure is encoded in [`scripts/e2e.sh`](../scripts/e2e.sh) — a single command. This doc explains the prereqs, what the script does at a high level, the architecture choices behind those steps, and how to debug when something breaks.

## Prerequisites

- Docker Desktop running (with bundled Kubernetes **disabled** — see Step 0 note below)
- `k3d` v5.8.3+ (`brew install k3d`), plus `kubectl`, `helm`, `grpcurl`
- Rust toolchain with the `aarch64-unknown-linux-musl` target
- Flutter SDK + Android command-line tools (for the emulator step) — see `docs/flutter-app.md`
- `MISTRAL_API_KEY` and `ANTHROPIC_API_KEY` set in the environment

The cluster runs on k3d (k3s in Docker). This is the supported runtime for sycophant local self-host because the transponder pod's `/etc/kernel` is a kubelet `hostPath` mount that requires the cluster node to see your host filesystem. Docker Desktop's bundled k8s does not expose `/Users` to its kind node, so it doesn't support the HostPath workflow out of the box.

## Running the e2e

```sh
./scripts/e2e.sh
```

The script bootstraps a fresh cluster, builds + loads all images, deploys the charts (Layer 1 — no headscale, no tsnet bridge), launches the Pixel Android emulator with the Flutter client, prints the enrollment code, pauses for the manual chat round-trip, and then runs the Step 6 security assertions. Re-running it deletes and recreates the cluster from scratch.

If a phase fails the script exits with `step_N_X failed`, leaving the cluster in place for inspection (the `EXIT` trap only kills the script's own background port-forward / flutter process, not the cluster).

## What the script does

| Phase | Function in script | What it lays down |
|---|---|---|
| 0 | `step_0_bootstrap` | k3d cluster → gVisor (runsc) → Cilium → Kyverno |
| 1 | `step_1_build` | Cross-compile Rust binaries → Docker build all images → k3d image import + push chambers to in-cluster registry |
| 2 | `step_2_configure` | Namespace, per-tenant TokenReview ClusterRoleBindings (Kyverno-generator workaround), mainframe kernel fixtures, LLM secrets, chamber fixtures |
| 3 | `step_3_deploy` | `helm install` cluster chart + tenant chart (Layer 1; the `clients.<name>.workspaces` block authorises the Flutter device against `hello-world`) |
| 4 | `step_4_verify` | Wait for hello-world workspace + controllers Ready; warn-only on `multi-agent` Pending (memory-constrained on Docker Desktop) |
| 5 | `step_5_flutter` | `kubectl port-forward 9091:9091` → poll Client CR `status.enrollmentCode` → launch `Pixel_9_API_36` → `flutter run` → pause for operator to enroll + chat |
| 6 | `step_6_security` | gVisor `dmesg` first line, secret-scrubbing count, airlock `exit_code=0`, NetworkPolicy egress timeout, no LLM creds in transponder pod, workspace SA exists |

## Architecture notes

**Why gVisor before Cilium.** The gVisor installer writes a containerd template and `HUP`s k3s to reload it. K3s embeds containerd as a subprocess, so the HUP restarts both. If Cilium is installed first, its agent's CRI socket disappears mid-restart and the DaemonSet enters CrashLoopBackOff. Installing gVisor first means the HUP fires when no DaemonSets depend on the CRI yet.

**Why kube-proxy stays + Cilium does CNI-only.** Cilium's full kube-proxy replacement (socket-LB based ClusterIP routing) doesn't work cleanly on k3d's containerd-2.0 + cgroup-v2 environment in 1.19.3 — pods can't reach ClusterIPs. With k3s's bundled kube-proxy retained, ClusterIP routing works out of the box. Cilium handles CNI + CiliumNetworkPolicy enforcement only.

**Why Kyverno is mandatory.** The cluster chart ships 4 ClusterPolicies + a ValidatingAdmissionPolicy. Without Kyverno's admission + background controllers, the policies install but never enforce. The mismatch only surfaces when downstream calls fail (e.g., airlock-ctrl SA tries `TokenReview` and the per-tenant ClusterRoleBinding the generator should have created doesn't exist).

**Why the TokenReview ClusterRoleBindings are minted manually.** The cluster chart's `tenant-rolebinding-generator` Kyverno policy currently matches namespaces named `tenant-*` created by the tenant-deployer SA. The e2e uses a static `e2e-test` namespace created directly, so the generator doesn't fire. Until that mismatch is resolved (open design item), Step 2 mints the two bindings itself.

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
kubectl rollout restart deployment tightbeam-controller -n e2e-test
```

### Turn stuck (no response after "received inbound message")
Check controller trace:
```sh
kubectl logs -n e2e-test deployment/tightbeam-ctrl
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
Tightbeam persists conversation history to `/var/log/tightbeam/<workspace>/`. Stale entries from a previous run can mislead the LLM on subsequent turns:

```sh
TBPOD=$(kubectl get pod -n e2e-test \
  -l app.kubernetes.io/name=tightbeam-ctrl -o name | head -1 | sed 's|pod/||')
kubectl debug -n e2e-test "$TBPOD" --image=busybox:1.36 \
  --target=ctrl --profile=general -it=false -- \
  rm -rf /proc/1/root/var/log/tightbeam/hello-world \
         /proc/1/root/var/log/tightbeam/multi-agent
kubectl rollout restart deployment tightbeam-controller -n e2e-test
```

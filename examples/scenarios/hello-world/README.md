# Hello World

The smallest end-to-end run: one workspace, one principal-authored system
prompt, one toolset — provisioned with the `syco` CLI, exercised from the
Flutter client, and closed out with a security audit. This is the reference
e2e: `setup` → `tenant up` → exercise (Flutter client) → `audit`.

## Prerequisites

- A sycophant checkout (the build runs from it) and Docker running.
- `syco` on your PATH (`cargo build -p syco`, then `target/debug/syco`).
- An LLM API key — examples below use OpenRouter (`$OPENROUTER_API_KEY`).

## 1. Cluster

`syco setup` is idempotent and from-nothing: it ensures the k3d cluster, the
gVisor runtime, Cilium, Kyverno, the toolset registry, and — from a checkout —
builds and loads the images.

```sh
syco setup
```

## 2. Stage kernel content

The harness reads the agent's `AGENTS.md` (and `agents/`, `skills/`) in-process
from a per-workspace read-only kernel volume it mounts — there is no separate
kernel-serving pod. You stage that content into the kernel source directory the
workspace's read-only PV points at: a host directory, and on local k3d the
cluster sees the path on your machine directly.

```sh
mkdir -p ~/sycophant/tmp/hello-world-data
cp examples/mainframe/simple/AGENTS.md ~/sycophant/tmp/hello-world-data/AGENTS.md
```

## 3. Tenant content

The LLM credential is applied via the CLI (kept out of chart values so platform
upgrades never prune it):

```sh
printf '%s' "$OPENROUTER_API_KEY" | syco tenant secret set openrouter --ns hello-world
```

## 4. Workspace + deploy

Toolsets, the workspace's kernel mount, and the toolset attachment all live in
the tenant values file. The `stdlib` toolset gives the workspace shell/file
tools and is the sandbox the audit probes; the `prompt` toolset's profile key is
the model the turn names. Seed the file from the scenario and set the absolute
hostPath:

```sh
mkdir -p ~/.config/sycophant/tenants/hello-world
cp examples/scenarios/hello-world/values.yaml \
  ~/.config/sycophant/tenants/hello-world/values.yaml
# edit the kernel.hostPath.path to: $HOME/sycophant/tmp/hello-world-data (absolute)

syco tenant up --ns hello-world
```

## 5. Exercise the workspace (audit fixture)

The toolset pod is lazy-spawned on the first tool call, so the audit needs a
message that makes the agent run a shell command. The CLI provisions; it does
not send messages — that's a client's job. Authorize a device, then drive it
from the Flutter client.

Authorize the device for this workspace and read the one-time enrollment code
the controller mints onto the Enrollment CR's status:

```sh
syco tenant enrollment set my-phone --workspace hello-world --ns hello-world
kubectl get enrollment my-phone -n hello-world -o jsonpath='{.status.enrollmentCode}'
```

Build, sideload, and enroll the app with that code per
[`docs/flutter-app.md`](../../../docs/flutter-app.md) (remote-device network
setup is in [`docs/headscale-self-host-acme.md`](../../../docs/headscale-self-host-acme.md)).
From the app's chat screen, send a message that triggers a tool call:

> Use your Bash tool to run: echo hello

## 6. Audit

Assert the workspace upholds the security clauses — gVisor isolation, secret
scrubbing, egress containment, the L7 DNS allowlist, credential isolation, tool
execution, and the workspace ServiceAccount:

```sh
syco tenant audit hello-world --ns hello-world
```

Exit 0 means every clause holds. If it reports the toolset pod is missing, the
message in step 5 didn't trigger a tool call — send another that uses Bash.

## Teardown

```sh
syco tenant remove --ns hello-world   # delete this tenant + its data
syco destroy                          # delete the whole cluster
```

# Hello World

The end-to-end run: one workspace, one principal-authored system prompt, two
toolsets — provisioned with the `syco` CLI, exercised from the Flutter client,
and closed out with a security audit. This is the reference e2e:
`setup` → `tenant up` → exercise (Flutter client) → `audit`.

The workspace carries the security fixtures because they are the foundation,
not an extra step:

- `stdlib` gives the workspace shell/file tools and is the gVisor sandbox the
  audit probes.
- `ssh-credentials` (see `examples/toolsets/ssh-credentials/`) exercises the
  harness's secret-scrubber: it exposes one tool — `test-cmd` — that emits the
  toolset-mounted SSH private key to stdout. When the LLM invokes it, the
  result flows back through the harness, where the scrubber replaces the key
  bytes with `[REDACTED:demo-ssh-key]` before anything lands in the
  conversation log. The tool is intentionally trivial and benign-sounding so
  the LLM invokes it on request without safety-refusing. The point is the
  path, not the payload.

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
cp examples/kernel/simple/AGENTS.md ~/sycophant/tmp/hello-world-data/AGENTS.md
```

## 3. Tenant content

The LLM credential and the demo SSH key the scrubber must redact are applied
via the CLI (kept out of chart values so platform upgrades never prune them):

```sh
printf '%s' "$OPENROUTER_API_KEY" | syco tenant secret set openrouter --ns hello-world
printf 'FAKE-ED25519-PRIVATE-KEY-DO-NOT-USE' | \
  syco tenant secret set demo-ssh-key --ns hello-world
```

The toolsets that consume them are declared in the tenant values file (step 4).

## 4. Workspace + deploy

Toolsets, the workspace's kernel mount, and the toolset attachment all live in
the tenant values file. The `prompt` toolset's profile key is the model the
turn names. Seed the file from the scenario, point the kernel at the seeded
directory (absolute path), then deploy:

```sh
mkdir -p ~/.config/sycophant/tenants/hello-world
cp examples/scenarios/hello-world/values.yaml \
  ~/.config/sycophant/tenants/hello-world/values.yaml
syco tenant kernel set hello-world --path $HOME/sycophant/tmp/hello-world-data --ns hello-world

syco tenant up --ns hello-world
```

## 5. Exercise the workspace (audit fixture)

The toolset pods are lazy-spawned on the first tool call, so the audit needs
messages that make the agent run its tools. The CLI provisions; it does not
send messages — that's a client's job. Authorize a device, then drive it from
the Flutter client.

Authorize the device for this workspace and read the one-time enrollment code
the controller mints onto the Enrollment CR's status:

```sh
syco tenant enrollment set my-phone --workspace hello-world --ns hello-world
kubectl get enrollment my-phone -n hello-world -o jsonpath='{.status.enrollmentCode}'
```

Build, sideload, and enroll the app with that code per
[`docs/flutter-app.md`](../../../docs/flutter-app.md) (remote-device network
setup is in [`docs/headscale-self-host-acme.md`](../../../docs/headscale-self-host-acme.md)).
From the app's chat screen, send a message that triggers a stdlib tool call:

> Use your Bash tool to run: echo hello

Then one that triggers the scrubber fixture:

> Use the test-cmd tool.

## 6. Audit

Assert the workspace upholds the security clauses — gVisor isolation, secret
scrubbing, egress containment, the L7 DNS allowlist, credential isolation, tool
execution, and the workspace ServiceAccount:

```sh
syco tenant audit hello-world --ns hello-world
```

Exit 0 means every clause holds. If it reports the toolset pod is missing, the
message in step 5 didn't trigger a tool call — send another that uses Bash.

## 7. Assertion — the key was scrubbed

This assertion is more specific than `syco tenant audit`'s generic clauses: it
proves *this* toolset-emitted secret was redacted, by name. The fake key must
NOT appear in the harness stdout; the scrubber redacts it before logging.
Expect `0`:

```sh
kubectl logs -n hello-world deployment/hello-world -c harness \
  | grep -c 'FAKE-ED25519-PRIVATE-KEY'
# expect: 0   (the bytes were replaced with [REDACTED:demo-ssh-key])
```

That check alone passes even if staging silently failed and the tool emitted
nothing, because `stage_credentials` only warns on a failed copy. Pair it with
the positive assertion. Expect at least `1`:

```sh
kubectl logs -n hello-world deployment/hello-world -c harness \
  | grep -c '\[REDACTED:demo-ssh-key\]'
# expect: >= 1   (the tool read the mounted key and emitted its bytes)
```

## Teardown

```sh
syco tenant remove --ns hello-world   # delete this tenant + its data
syco destroy                          # delete the whole cluster
```

# Toolset

[![made-with-rust](https://img.shields.io/badge/Made%20with-Rust-1f425f.svg)](https://www.rust-lang.org/)

The single worker-spawner for an agent workspace. Toolset is the one pod that spawns credentialed ephemeral Jobs: it reconciles the `Toolset`, `Model`, and `Provider` CRDs, serves gRPC to the Harness, and creates a short-lived Job for every tool call and every model call. It holds the sole `jobs:create` grant in the tenant namespace and never reads a credential — kubelet mounts secrets into the Jobs, not the controller.

## How It Works

One component, the **toolset controller** (`toolset-ctrl`), one per workspace namespace. It is the only gRPC server for the pod; the Harness and the spawned workers connect back to it as clients. It spawns two worker profiles over a set of predefined toolset images:

1. **Prompt worker** — the model call, reshaped as a toolset that runs a `prompt` tool. It pulls a turn assignment, calls the provider, and streams the result back over the worker path. This is the LLM dispatch that used to be a separate pod; there is no separate LLM-dispatch controller.

2. **Tool worker** — a toolset that executes one tool call. It pulls its assignment, execs the toolset image's fixed dispatcher (`/etc/toolset/dispatch <tool>`) with the validated arg values as env vars, and streams typed output frames back.

Both are ephemeral, credentialed Jobs. Both connect back to the controller, dequeue their assignment (long-poll), execute, stream frames, and exit. TTL cleanup reaps the completed pod (30s).

The premise is that an LLM call, a subagent, and a tool call are one mechanism — a toolset running a tool. The Harness fires the main turn's `prompt` call structurally inside its loop; the model cannot elect its own main turn. The model fires `prompt` only when it dispatches a subagent. Both land on the same turn-dispatch surface and spawn a prompt worker.

## Why Toolset

Spawning a credentialed pod is the sharpest privilege in the namespace. Collapsing tool execution and LLM dispatch onto one controller lets exactly one ServiceAccount hold `jobs:create`, so the history-owner (Harness) and the internet-facing pod (Relay) hold none.

- **One `jobs:create` holder** — the toolset controller's SA is the only holder of the `jobs` create verb in the tenant namespace. This is the load-bearing invariant the collapse exists to make true.
- **Credential containment** — the controller references Secrets by name in Job specs; kubelet mounts them into the ephemeral pod. The controller never sees a token, an SSH key, or an API key.
- **gVisor on the model call** — the model provider is untrusted, so its response stream is adversarial input. The code that parses it (SSE decode, tool-call extraction, content assembly) runs only inside the gVisor-contained prompt worker, never in the controller. gVisor contains a parser compromise the old runc+seccomp posture never did.
- **Per-provider egress pinning** — each prompt worker can egress only to the one provider it was spawned for, not the union of all configured providers.

## Architecture

```
                    gRPC (harness.toolset)
   Harness ───────────────────────────────> Controller
   (agent loop,        Turn / WatchTools          │
    conversation                                   │  creates k8s Jobs
    history)                                       │  (sole jobs:create)
                                          ┌────────┴────────┐
                    gRPC (toolset.toolset)│                 │
                                    Prompt Worker      Tool Worker
                                    (gVisor,           (gVisor,
                                     api key mounted)   dispatcher, creds)
                                          │                 │
                                          v                 v
                                    Provider API      external host
                                    (pinned FQDN)      (per-toolset FQDN)
```

The Harness dispatches on the controller's harness-facing surface. The controller resolves the assignment and spawns the matching worker Job. The worker dequeues, executes, and streams result frames back; the controller forwards them to the Harness on the open response stream. It persists nothing — the conversation log lives on the [Harness](harness.md).

## CRDs

The controller reconciles three kinds. `Toolset` drives tool dispatch; `Model` and `Provider` drive turn dispatch.

### Toolset

An image holding one or more tools. Tools are discovered from the image's `md.sycophant.tools` OCI label; each tool declares a structured arg schema the controller validates the model's input against before dispatch.

```yaml
apiVersion: sycophant.md/v1
kind: Toolset
metadata:
  name: git-ops
spec:
  image: ghcr.io/calebfaruki/toolset-git:latest
  credentials:
    - secret: git-ssh-key
      file: /root/.ssh/id_ed25519
  egress:
    - domain: github.com
      port: 22
  keepalive: false
```

The workspace PVC is always mounted RW at `/workspace`. `egress` trusts the registrable domain — `domain: github.com` covers `github.com` and `*.github.com`.

### Provider

An LLM API endpoint and the credential used to authenticate against it. One Provider can back many Models.

```yaml
apiVersion: sycophant.md/v1
kind: Provider
metadata:
  name: openrouter
spec:
  format: openai               # anthropic | openai | gemini
  baseUrl: https://openrouter.ai/api/v1
  secret:
    name: sycophant-llm-openrouter
    # key: api-key             # default; set only if the Secret uses another key
```

### Model

A specific model offered by a provider. `params` is free-form pass-through merged into the provider request body via RFC 7396 JSON Merge Patch; operator-bound fields (model, messages, system, tools, stream) are clobbered.

```yaml
apiVersion: sycophant.md/v1
kind: Model
metadata:
  name: deepseek-v4-flash
spec:
  providerRef:
    name: openrouter
  model: deepseek/deepseek-v4-flash
  params:
    max_tokens: 8192
```

The Secret holds one value: the API key. Kubelet projects it into the prompt worker at `/run/secrets/toolset/api-key`; the controller never reads it. See [`docs/secrets-providers.md`](secrets-providers.md) for backend recipes.

## The Fold: an LLM Call Is a Toolset

The prompt worker fetches its work and returns its result over the same worker-dispatch surface the tool workers use. The worker surface carries both vocabularies, disjoint by design: the tool-call assignment (call id, working dir, args) with its stdout/stderr/outcome frames, and the turn assignment (system, tools, messages, merged params) with its content-delta / tool-use / turn-complete frames.

The prompt worker obtains gVisor by carrying the same pod label the tool workers carry — `app.kubernetes.io/component: tool-job` — plus the non-empty tenant workspace label the gVisor ValidatingAdmissionPolicy requires. It thereby falls under the existing gVisor Kyverno mutate (which stamps `runtimeClassName: gvisor`) and the VAP with no change to either cluster policy. The gVisor gate is not broadened; the prompt worker is reshaped into the already-gated toolset shape.

The neutral message vocabulary (`ContentBlock`, `Message`, `ToolCall`, `ToolDefinition`, `StopReason`, turn request/result) lives once, as the proto types. The `model-provider` parsers depend on and emit those shapes; the on-disk conversation log serializes them; the wire carries them — so the log and the wire cannot diverge. The proto content block carries a `FileBlock` variant for incoming files.

## Per-Provider Egress

Each configured provider gets its own prompt toolset, `prompt-<provider>`, whose egress is statically pinned to that one provider's FQDN. The operator declares one prompt toolset per provider (`prompt-openrouter`, `prompt-anthropic`, …), each carrying its provider FQDN in `spec.egress[]`.

The pin is authored by the same per-toolset mechanism the tool toolsets use. `syco toolset set`, running out-of-tenant under the operator kubeconfig at provisioning time, writes one CiliumNetworkPolicy per toolset (`toolset-<name>`), keyed on the `sycophant.md/toolset:<name>` pod label. No controller authors a policy, no in-namespace SA gains a `networkpolicies`/`ciliumnetworkpolicies` verb, and no per-spawn policy is generated at runtime.

Each per-toolset CNP composes additively on the chart's `tool-job-baseline` floor — a fail-closed policy selecting every `tool-job` pod that allows only kube-dns:53 (L7 DNS allowlist pinned to the `toolset-ctrl` FQDN) and `toolset-ctrl:9090`. A worker with no per-toolset CNP therefore reaches nothing external.

At spawn time the controller resolves which prompt toolset a turn's model maps to (from the model's provider) and spawns that toolset. The map fails closed: a model whose provider has no registered `prompt-<provider>` toolset refuses the turn — never a fallback to a default toolset or a union allowance. A prompt toolset's name must not collide with any tool toolset's name (a shared name would union their egress under one selector); the `prompt-` prefix keeps them distinct.

## gRPC Protocol

Single service: `toolset.v1.ToolsetController`. Proto at `crates/toolset-proto/proto/toolset/v1/toolset.proto`; shared message types at `sycophant/common/v1/common.proto`. Four surfaces on one listener (`:9090`, internal-only):

| RPC | Caller | Surface |
|-----|--------|---------|
| `Turn` | Harness | Turn dispatch: send a turn, stream turn events |
| `CancelTurn` | Harness | Turn dispatch: cancel an in-flight turn |
| `WatchTools` | Harness | Tool dispatch: subscribe to the tool-list snapshot + changes |
| `BeginToolCall` | Harness | Tool dispatch: dispatch a tool call, get its `call_id` |
| `AwaitToolResult` | Harness | Tool dispatch: subscribe to a call's typed output frames |
| `CancelToolCall` | Harness | Tool dispatch: cancel an in-flight tool call |
| `GetTurn` | Prompt worker | Pull a turn assignment (long-poll) |
| `StreamTurnResult` | Prompt worker | Client-stream the turn's result chunks back |
| `AwaitTurnCancel` | Prompt worker | Long-poll for a cancel of the in-flight turn |
| `GetToolCall` | Tool worker | Pull a tool-call assignment (long-poll) |
| `StreamToolResult` | Tool worker | Client-stream the executed call's output frames |
| `AwaitToolCancel` | Tool worker | Long-poll for a cancel of the in-flight call |

A cancel from the Harness reaches the running worker through the worker-side long-poll (`AwaitTurnCancel` / `AwaitToolCancel`), which lets the worker abandon its in-flight provider call or SIGKILL its child.

## RBAC

The controller ServiceAccount can create Jobs, read the three CRD registries, emit Events, and authenticate caller SA tokens. It has **zero access to Secrets** — credential Secrets are kubelet-mounted into worker pods and never seen by the controller.

```yaml
rules:
  - apiGroups: ["batch"]
    resources: ["jobs"]
    verbs: ["create", "get", "list", "watch", "delete"]
  - apiGroups: ["sycophant.md"]
    resources: ["toolsets"]
    verbs: ["get", "list", "watch"]
  - apiGroups: ["sycophant.md"]
    resources: ["toolsets/status"]
    verbs: ["patch"]
  - apiGroups: ["sycophant.md"]
    resources: ["models", "providers"]
    verbs: ["get", "list", "watch"]
  - apiGroups: [""]
    resources: ["events"]
    verbs: ["create"]
```

This is the only `jobs:create` grant in the tenant namespace; the Harness and Relay grant no `jobs` verb. TokenReview is cluster-scoped: a shared `cluster-toolset-tokenreview` ClusterRole grants `create` on `tokenreviews`, bound to each tenant's `toolset-ctrl` SA by the `tenant-rolebinding-generator` Kyverno policy.

## Security Model

- The controller holds the sole `jobs:create` in the namespace; a compromised Harness or Relay cannot spawn a credentialed pod.
- The controller has zero Secret RBAC. Credentials exist only in ephemeral worker pods, mounted by kubelet. The provider API key is a file (`/run/secrets/toolset/api-key`, mode 0o440), never an env var, and never appears in a Job spec, a gRPC message, or controller memory.
- Both worker profiles run under gVisor, gated solely by the `tool-job` component label. The adversarial provider-stream parser is contained.
- Two-tier audience gate, verified by K8s TokenReview: harness-facing methods require the `harness.toolset` audience; the six worker-dispatch methods require `toolset.toolset`. The worker audience is minted only on the worker Job pod; a stolen Harness token cannot reach worker methods, and vice versa. Relay presents `relay.toolset` when it dials the controller.
- Each prompt worker's egress is pinned to its own provider's FQDN by a static per-toolset CNP layered on the fail-closed `tool-job-baseline` floor. There is no shared-component union egress policy.
- Tool arg values flow to the toolset dispatcher as env vars, never argv; the dispatcher's `"$VAR"` expansion is the only string-to-shell crossing, and the model never authors a shell command.
- Secret values (raw, base64, URL-encoded) are scrubbed from worker output before it crosses the gRPC boundary.
- Worker pods set `shareProcessNamespace: false`, `automountServiceAccountToken: false`, and a hardened security context (non-root, read-only rootfs, all capabilities dropped).

## Crate Structure

```
crates/
  toolset-proto/       # gRPC proto definitions (toolset.v1)
  toolset-controller/  # the toolset-ctrl controller binary
  toolset-runtime/     # in-toolset execution runtime (tool workers)
  prompt-toolset/      # the prompt worker binary (LLM call as a toolset)
  model-provider/      # provider dialect parsers (claude, openai, gemini)
```

`toolset-runtime` is the base runtime baked into every tool image: it connects back to the controller, receives the validated arg map, execs the dispatcher, and streams frames. `prompt-toolset` is the superset image — it adds `model-provider` and the `prompt` tool. All provider dialects are bundled in the one prompt image; a spawn drives exactly the dialect matching the resolved provider format. Per-dialect images would triple the build surface for no security gain, since gVisor and the per-Job secret mount already contain the blast radius.

## Installation

Container images are published to GHCR on each release:

```
ghcr.io/calebfaruki/toolset-controller:latest
ghcr.io/calebfaruki/prompt-toolset:latest
ghcr.io/calebfaruki/toolset-git:latest
```

CRDs (`Toolset`, `Model`, `Provider`) ship in the cluster chart (`charts/sycophant-cluster/crds/`) and are installed once per cluster. The per-tenant chart (`charts/sycophant-tenant/`) installs the controller in each workspace namespace. Then:

```bash
syco toolset set git-ops --image ghcr.io/calebfaruki/toolset-git:latest \
  --credential secret=git-ssh-key,file=/root/.ssh/id_ed25519 --egress github.com:22
syco toolset set prompt-openrouter --image ghcr.io/calebfaruki/prompt-toolset:latest \
  --egress openrouter.ai:443
syco model set deepseek/deepseek-v4-flash --provider openrouter --secret sycophant-llm-openrouter
```

`syco toolset lint <dir>` statically checks a toolset directory's dispatcher and Makefile for shell-injection patterns before you build the image.
</content>
</invoke>

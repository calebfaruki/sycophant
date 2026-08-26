# Toolset

[![made-with-rust](https://img.shields.io/badge/Made%20with-Rust-1f425f.svg)](https://www.rust-lang.org/)

The single tool-job spawner for an agent workspace. Toolset is the one pod that spawns credentialed ephemeral Jobs: it reads its toolset config from a chart-rendered ConfigMap, serves gRPC to the Harness, and creates a short-lived Job for every tool call and every model call. It holds the sole `jobs:create` grant in the tenant namespace and never reads a credential — kubelet mounts secrets into the Jobs, not the controller.

## How It Works

One component, the **toolset controller** (`toolset-ctrl`), one per workspace namespace. It is the only gRPC server for the pod; the Harness and the spawned tool jobs connect back to it as clients. It spawns two tool-job kinds over a set of predefined toolset images:

1. **Prompt job** — the model call, reshaped as a toolset that runs a `prompt` tool. It pulls a turn assignment, calls the provider, and streams the result back over the tool-job path. This is the LLM dispatch that used to be a separate pod; there is no separate LLM-dispatch controller.

2. **Tool job** — a toolset that executes one tool call. It pulls its assignment, execs the toolset image's fixed dispatcher (`/etc/toolset/dispatch <tool>`) with the validated arg values as env vars, and streams typed output frames back.

Both are ephemeral, credentialed Jobs. Both connect back to the controller, dequeue their assignment (long-poll), execute, stream frames, and exit. TTL cleanup reaps the completed pod (30s).

The premise is that an LLM call, a subagent, and a tool call are one mechanism — a toolset running a tool. The Harness fires the main turn's `prompt` call structurally inside its loop; the model cannot elect its own main turn. The model fires `prompt` only when it dispatches a subagent. Both land on the same turn-dispatch surface and spawn a prompt job.

## Why Toolset

Spawning a credentialed pod is the sharpest privilege in the namespace. Collapsing tool execution and LLM dispatch onto one controller lets exactly one ServiceAccount hold `jobs:create`, so the history-owner (Harness) and the internet-facing pod (Relay) hold none.

- **One `jobs:create` holder** — the toolset controller's SA is the only holder of the `jobs` create verb in the tenant namespace. This is the load-bearing invariant the collapse exists to make true.
- **Credential containment** — the controller references Secrets by name in Job specs; kubelet mounts them into the ephemeral pod. The controller never sees a token, an SSH key, or an API key.
- **gVisor on the model call** — the model provider is untrusted, so its response stream is adversarial input. The code that parses it (SSE decode, tool-call extraction, content assembly) runs only inside the gVisor-contained prompt job, never in the controller. gVisor contains a parser compromise the old runc+seccomp posture never did.
- **Per-provider egress pinning** — each prompt job can egress only to the one provider it was spawned for, not the union of all configured providers.

## Architecture

```
                    gRPC (harness.toolset)
   Harness ───────────────────────────────> Controller
   (agent loop,        Turn / WatchTools          │
    conversation                                   │  creates k8s Jobs
    history)                                       │  (sole jobs:create)
                                          ┌────────┴────────┐
                       gRPC (tool.toolset)│                 │
                                    Prompt Job         Tool Job
                                    (gVisor,           (gVisor,
                                     api key mounted)   dispatcher, creds)
                                          │                 │
                                          v                 v
                                    Provider API      external host
                                    (pinned FQDN)      (per-toolset FQDN)
```

The Harness dispatches on the controller's harness-facing surface. The controller resolves the assignment and spawns the matching tool Job. The tool job dequeues, executes, and streams result frames back; the controller forwards them to the Harness on the open response stream. It persists nothing — the conversation log lives on the [Harness](harness.md).

## Toolset Configuration

Toolsets are chart values, not CRs. `charts/sycophant-tenant` renders the
`toolsets` map into a `toolset-config` ConfigMap; the controller reads it once at
startup. Changing it rolls the controller pod.

An entry is flat. `image` and `keepalive` are read by the controller: `image`
selects the tool job's pod, `keepalive` sets the Job restart policy and idle-reap.
Neither is forwarded to the tool job.

An entry owns no credential and no network hole. Both come from the binding
workspace's grant menu, so one generic toolset definition serves every
workspace. `env` keys are forwarded into the tool job verbatim as env vars.

### A tool toolset

An image holding one or more tools. Tools are discovered from the image's
`md.sycophant.tools` OCI label; each tool declares a structured arg schema the
controller validates the model's input against before dispatch.

```yaml
toolsets:
  git-ops:
    image: ghcr.io/calebfaruki/toolset-git:latest
    keepalive: false
```

The workspace PVC is always mounted RW at `/workspace`.

### Grants

A workspace binds a toolset either by bare name or by an object carrying a
grant menu. A grant is one operator-approved credential scoped to that
(workspace, toolset) pair: it names a Kubernetes Secret, and optionally a `path`
where the credential file lands and one `egress` domain.

```yaml
workspaces:
  research:
    toolsets:
      - name: git-ops
        grants:
          deploy-key:
            secret: research-git-ssh-key
            path: /home/agent/.ssh/id_ed25519
          github:
            secret: research-github-token
            egress: github.com
```

A tool call selects one grant by name from that menu; a name outside it is
refused and no Job is created.

**The human selects, not the model.** The client reads the menu from the Relay
(`ListGrants`, names only) and attaches the user's choices to the message
it sends, one grant per toolset. The Harness injects the selection into each
tool call it dispatches to that toolset, and strips any `__grant` the model
wrote before injecting its own, so a model-authored selection can never reach
the controller. A message that selects nothing dispatches grantless: no
credential, baseline egress. A keepalive pod holds the credential it was
spawned with, so a call selecting a different grant replaces that pod rather
than reusing it.

The Secret must carry its value under a data key equal to the Secret's own
name. The credential is mounted read-only at a staging
path and copied to its target at mode `0o600` before the first tool runs; with
no `path` it lands at `/run/secrets/grant/credential`. A `path` may not shadow
the projected ServiceAccount token mount, anything under `/etc/toolset`, or
`/workspace`.

`egress` is optional and names exactly one domain, rendered into a per-grant
CiliumNetworkPolicy selecting the workspace, toolset, and grant labels together.
It matches that host and no subdomains. The domain renders an L7 `rules.dns`
entry on `:53` plus a `toFQDNs` rule on `:443`, so a bare IP literal is not
expressible — use a name the cluster's DNS resolves. A grant that declares no
`egress` mounts its secret and opens nothing, staying on the fail-closed
baseline floor.

### The prompt toolset

The prompt toolset is the hardcoded turn server, so it is not an entry of the
`toolsets` map and no workspace binds it. It gets its own values section, read
directly by the controller. A profile key is the turn's `model` value; a `model`
absent from the map is rejected, never defaulted. Each profile pins one provider
endpoint, its credential, and its egress.

```yaml
prompt:
  profiles:
    deepseek-v4-flash:
      image: ghcr.io/calebfaruki/prompt-toolset:latest
      format: openai
      model: deepseek/deepseek-v4-flash
      baseUrl: https://openrouter.ai/api/v1
      secret: sycophant-llm-openrouter
      egress:
        - { domain: openrouter.ai, port: 443 }
```

The Secret holds one value: the API key. Kubelet projects it into the prompt
job at the declared path; the controller never reads it. See
[`docs/secrets.md`](secrets.md) for backend recipes.

`secret` is the one optional key. A `baseUrl` inside the cluster authenticates
nobody, so its profile omits `secret` and the prompt job spawns with no
credential volume and nothing registered to scrub.

## The Fold: an LLM Call Is a Toolset

The prompt job fetches its work and returns its result over the same tool-job dispatch surface every other tool job uses. The tool-job surface carries both vocabularies, disjoint by design: the tool-call assignment (call id, working dir, args) with its stdout/stderr/outcome frames, and the turn assignment (system, tools, messages, merged params) with its content-delta / tool-use / turn-complete frames.

The prompt job obtains gVisor by carrying the same pod label the tool jobs carry — `app.kubernetes.io/component: tool-job` — plus the non-empty tenant workspace label the gVisor ValidatingAdmissionPolicy requires. It thereby falls under the existing gVisor Kyverno mutate (which stamps `runtimeClassName: gvisor`) and the VAP with no change to either cluster policy. The gVisor gate is not broadened; the prompt job is reshaped into the already-gated toolset shape.

The neutral message vocabulary (`ContentBlock`, `Message`, `ToolCall`, `ToolDefinition`, `StopReason`, turn request/result) lives once, as the proto types. The `model-provider` parsers depend on and emit those shapes; the on-disk conversation log serializes them; the wire carries them — so the log and the wire cannot diverge. The proto content block carries a `FileBlock` variant for incoming files.

## Per-Profile Egress

Each profile gets its own CiliumNetworkPolicy, `toolset-<profile-key>`, keyed on
the `sycophant.md/toolset: <profile-key>` pod label. The chart renders it from
the profile's `egress` list at install time. No controller authors policy, no
in-namespace ServiceAccount gains a `networkpolicies`/`ciliumnetworkpolicies`
verb, and no per-spawn policy is generated at runtime.

Each per-profile CNP composes additively on the chart's `tool-job-baseline`
floor — a fail-closed policy selecting every `tool-job` pod that allows only
kube-dns:53 (L7 DNS allowlist pinned to the `toolset-ctrl` FQDN) and
`toolset-ctrl:9090`. A tool or prompt job with no per-profile CNP therefore reaches nothing
external.

At spawn time the controller resolves the profile the turn's `model` names and
spawns that prompt job. The map fails closed: a `model` with no profile refuses the
turn — never a fallback to a default profile or a union allowance. Because a
prompt profile is one entry in one selector-keyed CNP, two providers can never
share an egress allowance.

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
| `GetTurn` | Prompt job | Pull a turn assignment (long-poll) |
| `StreamTurnResult` | Prompt job | Client-stream the turn's result chunks back |
| `AwaitTurnCancel` | Prompt job | Long-poll for a cancel of the in-flight turn |
| `GetToolCall` | Tool job | Pull a tool-call assignment (long-poll) |
| `StreamToolResult` | Tool job | Client-stream the executed call's output frames |
| `AwaitToolCancel` | Tool job | Long-poll for a cancel of the in-flight call |

A cancel from the Harness reaches the running tool job through the tool-job-side long-poll (`AwaitTurnCancel` / `AwaitToolCancel`), which lets it abandon its in-flight provider call or SIGKILL its child.

## RBAC

The controller ServiceAccount can create Jobs and emit Events. It reads no CRDs: the toolset config arrives as a mounted ConfigMap. It has **zero access to Secrets** — credential Secrets are kubelet-mounted into the tool-job pods and never seen by the controller.

```yaml
rules:
  - apiGroups: ["batch"]
    resources: ["jobs"]
    verbs: ["create", "get", "list", "watch", "delete"]
  - apiGroups: [""]
    resources: ["events"]
    verbs: ["create"]
```

This is the only `jobs:create` grant in the tenant namespace; the Harness and Relay grant no `jobs` verb. TokenReview is cluster-scoped: a shared `cluster-toolset-tokenreview` ClusterRole grants `create` on `tokenreviews`, bound to each tenant's `toolset-ctrl` SA by the `tenant-rolebinding-generator` Kyverno policy.

## Security Model

- The controller holds the sole `jobs:create` in the namespace; a compromised Harness or Relay cannot spawn a credentialed pod.
- The controller has zero Secret RBAC. Credentials exist only in ephemeral tool-job pods, placed there by kubelet. A resolved grant is delivered as a file and never as an environment variable: env leaks through `/proc/<pid>/environ`, child process inheritance, and logs. The Job spec carries only a reference — the credential value never appears in a Job spec, a gRPC message, or controller memory.
- A tool job holds at most one credential, selected per call from the closed set its workspace binds, so a hijacked job holds one credential that works against one destination.
- Both tool-job kinds run under gVisor, gated solely by the `tool-job` component label. The adversarial provider-stream parser is contained.
- Two-tier audience gate, verified by K8s TokenReview: harness-facing methods require the `harness.toolset` audience; the six tool-job-dispatch methods require `tool.toolset`. The tool-job audience is minted only on the tool-job pods; a stolen Harness token cannot reach tool-job methods, and vice versa. Relay never dials the controller: it reaches the workspace through the Harness, presenting `relay.harness`.
- Each prompt job's egress is pinned to its own provider's FQDN by a static per-profile CNP layered on the fail-closed `tool-job-baseline` floor. There is no shared-component union egress policy.
- Tool arg values flow to the toolset dispatcher as env vars, never argv; the dispatcher's `"$VAR"` expansion is the only string-to-shell crossing, and the model never authors a shell command.
- Secret values (raw, base64, URL-encoded) are scrubbed from tool-job output before it crosses the gRPC boundary.
- Tool-job pods set `shareProcessNamespace: false`, `automountServiceAccountToken: false`, and a hardened security context (non-root, read-only rootfs, all capabilities dropped).

## Crate Structure

```
crates/
  toolset-proto/       # gRPC proto definitions (toolset.v1)
  toolset-controller/  # the toolset-ctrl controller binary
  toolset-runtime/     # in-toolset execution runtime (tool jobs)
  prompt-toolset/      # the prompt job binary (LLM call as a toolset)
  model-provider/      # provider dialect parsers (claude, openai, gemini)
```

`toolset-runtime` is the entrypoint of the toolset base image (`images/toolset-base/`): it connects back to the controller, receives the validated arg map, execs the dispatcher, and streams frames. Every toolset image builds `FROM` that base and adds only its tools; separate images exist for blast radius, not for the runtime. The prompt image builds `FROM` the same base and overrides the entrypoint with `prompt-toolset`, which drives the turn verbs instead of the tool verbs. All provider dialects are bundled in the one prompt image; a spawn drives exactly the dialect matching the resolved provider format. Per-dialect images would triple the build surface for no security gain, since gVisor and the per-Job secret mount already contain the blast radius.

## Installation

No images are published yet. `syco setup` builds them from the checkout and
loads them into the local cluster: controller images straight into the k3d
node (`:local` tags), toolset images via the in-cluster registry
(`sycophant-registry:5000/<name>:latest`). The stdlib toolset image is
`toolset`; the base every toolset builds `FROM` is `toolset-base`.

The per-tenant chart (`charts/sycophant-tenant/`) installs the controller in each workspace namespace. Declare the toolsets in that chart's values:

```yaml
toolsets:
  git-ops:
    image: ghcr.io/calebfaruki/toolset-git:latest

workspaces:
  research:
    toolsets:
      - name: git-ops
        grants:
          deploy-key:
            secret: research-git-ssh-key
            path: /home/agent/.ssh/id_ed25519

prompt:
  profiles:
    deepseek-v4-flash:
      image: ghcr.io/calebfaruki/prompt-toolset:latest
      format: openai
      model: deepseek/deepseek-v4-flash
      baseUrl: https://openrouter.ai/api/v1
      secret: sycophant-llm-openrouter
      egress:
        - { domain: openrouter.ai, port: 443 }
```

`syco toolset lint <dir>` statically checks a toolset directory's dispatcher and Makefile for shell-injection patterns before you build the image.
</content>
</invoke>

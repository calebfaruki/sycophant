# Toolset

[![made-with-rust](https://img.shields.io/badge/Made%20with-Rust-1f425f.svg)](https://www.rust-lang.org/)

A toolset is a credentialed capability an agent workspace can call. The Harness is the one pod that spawns credentialed ephemeral Jobs: it reads its tool catalog once at boot from a chart-rendered ConfigMap, creates a short-lived Job for every tool call and every remote model call, and dials the serving Job pod directly. It holds the sole `jobs:create` grant in the tenant namespace and never reads a credential — kubelet mounts secrets into the Jobs, not the Harness.

## How It Works

The Harness creates and dials every capability Job. There is no separate controller pod: the Harness reads its resolved tool catalog once at boot from the mounted `capability-manifest-<workspace>` ConfigMap, and from then on it spawns and dials Jobs itself. It creates two capability-job kinds over a set of predefined toolset images:

1. **Inference job** — the remote model call. For a remote `baseUrl` the Harness spawns an `inference-runtime` Job, dials it, and streams the model's result events back. This is the LLM dispatch that used to be a separate pod; there is no separate LLM-dispatch controller.

2. **Tool job** — a toolset that executes one tool call. It execs the toolset image's fixed dispatcher (`/etc/toolset/dispatch <tool>`) with the validated arg values as env vars, and streams typed output frames back.

Both are ephemeral, credentialed Jobs. The Harness dials each pod directly and opens one bidirectional `Run` stream per call: it sends the assignment as the first message, the pod streams its result frames back, and the Harness pushes a cancel on the same held stream. TTL cleanup reaps the completed pod (30s).

The premise is that an LLM call, a subagent, and a tool call are one mechanism — a capability Job the Harness dials. The Harness fires the main turn's model call structurally inside its loop; the model cannot elect its own main turn. The model fires a delegate turn only when it dispatches a subagent. Both land on the same turn-dispatch surface.

## Why Toolset

Spawning a credentialed pod is the sharpest privilege in the namespace. Making the Harness the one pod that creates Jobs lets exactly one ServiceAccount hold `jobs:create`, so the internet-facing pod (Relay) holds none and the spawned pods hold none.

- **One `jobs:create` holder** — the `harness-<workspace>` Role is the only holder of the `jobs` create verb in the tenant namespace. This is the load-bearing invariant.
- **Credential containment** — the Harness references Secrets by name in Job specs; kubelet mounts them into the ephemeral pod. The Harness never sees a token, an SSH key, or an API key, and holds no Secret RBAC.
- **gVisor on the model call** — the model provider is untrusted, so its response stream is adversarial input. The code that parses it (SSE decode, tool-call extraction, content assembly) runs only inside the gVisor-contained inference job, never in the Harness. gVisor contains a parser compromise the old runc+seccomp posture never did.
- **Per-provider egress pinning** — each inference job can egress only to the one provider it was spawned for, not the union of all configured providers.

## Architecture

```
   Harness ──────────────────────────────────────────────┐
   (agent loop,        creates k8s Jobs (sole jobs:create) │
    conversation       reads catalog from mounted ConfigMap │
    history,                                                │  dials each pod
    dials pods)                                             │  (headless per-pod DNS)
                                          ┌─────────────────┴─────────────┐
                       gRPC (ToolJob.Run  │                               │
                        / InferenceJob.Run│                               │
                         harness-dialed)  v                               v
                                    Inference Job                     Tool Job
                                    (gVisor,                          (gVisor,
                                     api key mounted)                  dispatcher, creds)
                                          │                               │
                                          v                               v
                                    Provider API                    external host
                                    (pinned FQDN)                    (per-toolset FQDN)
```

The Harness spawns the matching capability Job and dials it over a headless per-workspace Service (`capability-<workspace>`), whose per-pod DNS record is the pod's call id, so the Harness reaches each pod directly rather than a load-balanced VIP. The pod dequeues nothing — it serves the `Run` stream the Harness opens, executes, and streams result frames back on the same connection. It persists nothing — the conversation log lives on the [Harness](harness.md).

## Toolset Configuration

Toolsets are chart values, not CRs. `charts/sycophant-tenant` renders the
`toolsets` map into a `toolset-config` ConfigMap and each workspace's resolved
catalog into a `capability-manifest-<workspace>` ConfigMap; the Harness reads
them once at startup. Changing them rolls the Harness pod.

An entry is flat. `image` and `keepalive` are read by the Harness: `image`
selects the tool job's pod, `keepalive` sets the Job restart policy and idle-reap.
Neither is forwarded to the tool job.

An entry owns no credential and no network hole. Both come from the binding
workspace's grants, so one generic toolset definition serves every
workspace. `env` keys are forwarded into the tool job verbatim as env vars.

### A tool toolset

An image holding one or more tools. Tools are discovered from the image's
`md.sycophant.tools` OCI label; each tool declares a structured arg schema the
Harness validates the model's input against before dispatch.

```yaml
toolsets:
  git-ops:
    image: ghcr.io/calebfaruki/toolset-git:latest
    keepalive: false
```

The workspace PVC is always mounted RW at `/workspace`.

### Grants

A workspace binds a toolset either by bare name or by an object carrying
grants. A grant is one operator-approved credential scoped to that
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

A tool call selects one grant by name from those grants; a name outside it is
refused and no Job is created.

**The human selects, not the model.** The client reads the grants from the Relay
(`ListGrants`, names only) and attaches the user's choices to the message
it sends, one grant per toolset. The Harness injects the selection into each
tool call it dispatches to that toolset, and strips any `__grant` the model
wrote before injecting its own, so a model-authored selection can never change
the credential. A message that selects nothing dispatches grantless: no
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
namespace default-deny egress floor.

### The model configuration

The turn destinations are not entries of the `toolsets` map and no workspace
binds them. They get their own `model` values section, read directly by the
harness. A key is the turn's `model` value; a `model` absent from the map is
rejected, never defaulted. Each entry pins one provider endpoint and its
credential.

```yaml
model:
  deepseek-v4-flash:
    image: ghcr.io/calebfaruki/inference-runtime:latest
    format: openai
    model: deepseek/deepseek-v4-flash
    baseUrl: https://openrouter.ai/api/v1
    secret: sycophant-llm-openrouter
```

The Secret holds one value: the API key. Kubelet projects it into the inference
job at the declared path; the Harness never reads it. See
[`docs/secrets.md`](secrets.md) for backend recipes.

`secret` is the one optional key. A `baseUrl` inside the cluster authenticates
nobody, so its entry omits `secret` and the inference job spawns with no
credential volume and nothing registered to scrub. A `baseUrl` naming an
in-cluster inference Service takes no job at all: the harness dials the warm
Service directly.

## The Model Call

The harness owns the model call. It resolves the turn's `model` against the
`model` configuration and dials the destination directly. A local `baseUrl` (an
in-cluster inference Service) is called in-process over the Service's own fence.

A remote `baseUrl` takes a per-call `inference-runtime` Job the harness creates
and dials over a bidirectional `InferenceJob.Run` stream: the harness sends the
turn assignment (system, tools, messages) and reads the model's content-delta /
tool-use / turn-complete events back on the held stream.

The inference job obtains gVisor by carrying the same pod label the tool jobs carry — `app.kubernetes.io/component: capability-job` — plus the non-empty tenant workspace label the gVisor ValidatingAdmissionPolicy requires. It thereby falls under the existing gVisor Kyverno mutate (which stamps `runtimeClassName: gvisor`) and the VAP with no change to either cluster policy. The gVisor gate is not broadened.

The neutral message vocabulary (`ContentBlock`, `Message`, `ToolCall`, `ToolDefinition`, `StopReason`, turn request/result) lives once, as the proto types. The `model-provider` parsers depend on and emit those shapes; the on-disk conversation log serializes them; the wire carries them — so the log and the wire cannot diverge. The proto content block carries a `FileBlock` variant for incoming files.

## Per-Model Egress

Each model gets its own CiliumNetworkPolicy, `inference-egress-<model-key>`,
keyed on the `sycophant.md/model: <model-key>` and `sycophant.md/job-kind:
inference` pod labels the harness stamps on the inference job. The chart renders
it for the model at install time. No controller authors policy, no in-namespace
ServiceAccount gains a `networkpolicies`/`ciliumnetworkpolicies` verb, and no
per-spawn policy is generated at runtime.

Each per-model CNP composes additively on the namespace `default-deny-egress`
floor. A tool or inference job with no per-model CNP therefore reaches nothing
external. Every kube-dns:53 rule the CNP emits carries its own L7 `rules.dns`
allowlist alongside its L4 ports: Cilium unions same-PortProtocol L7 DNS rules
across policies, so one L4-only kube-dns rule would nullify every sibling
policy's pinned allowlist and silently reopen DNS-tunnel exfiltration.

The map fails closed: a `model` with no entry refuses the turn — never a
fallback to a default or a union allowance. Because a model is one entry in one
selector-keyed CNP, two providers can never share an egress allowance.

## gRPC Protocol

Two harness-dialed, pod-served capability surfaces, defined in
`crates/toolset-proto/proto/toolset/v1/toolset.proto`; shared message types at
`sycophant/common/v1/common.proto`. The Harness is the client on both; each
serving pod listens on `:9090`, internal-only, reachable only by the Harness.

| Service / RPC | Caller | Surface |
|-----|--------|---------|
| `ToolJob.Run` | Harness | Bidi stream: send a tool-call assignment first, read the executed call's typed output frames, push a cancel on the same outbound half |
| `InferenceJob.Run` | Harness | Bidi stream: send the turn assignment first, read the model's turn events, push a cancel on the same outbound half |

One held connection per call carries the assignment, the result frames, and any cancel, so no separate pod-initiated call survives. A cancel from the Harness reaches the running capability job on the same outbound stream half, which lets it abandon its in-flight provider call or SIGKILL its child.

## RBAC

The `harness-<workspace>` Role can create Jobs, and holds no other verb on any
resource. It reads no CRDs: the tool catalog arrives as a mounted ConfigMap. It
has **zero access to Secrets** — credential Secrets are kubelet-mounted into the
capability-job pods and never seen by the Harness.

```yaml
rules:
  - apiGroups: ["batch"]
    resources: ["jobs"]
    verbs: ["create"]
```

This is the only `jobs:create` grant in the tenant namespace; the Relay grants
no `jobs` verb. `create` on Jobs is safe only because the identity-keyed
capability-job admission gate pins the resulting pod to the zero-RBAC
`unprivileged-<workspace>` ServiceAccount and forces the isolation envelope on
the same admission event.

## Security Model

- The `harness-<workspace>` Role holds the sole `jobs:create` in the namespace; a compromised Relay cannot spawn a credentialed pod.
- The Harness has zero Secret RBAC. Credentials exist only in ephemeral capability-job pods, placed there by kubelet. A resolved grant is delivered as a file and never as an environment variable: env leaks through `/proc/<pid>/environ`, child process inheritance, and logs. The Job spec carries only a reference — the credential value never appears in a Job spec, a gRPC message, or Harness memory.
- A tool job holds at most one credential, selected per call from the closed set its workspace binds, so a hijacked job holds one credential that works against one destination.
- Both capability-job kinds run under gVisor, gated solely by the `capability-job` component label. The adversarial provider-stream parser is contained.
- The capability-job pod runs as the zero-RBAC `unprivileged-<workspace>` ServiceAccount with no automounted token, so a subverted tool inherits neither a verb nor a bearer token. The Harness dialed the pod and owns the connection, so a subverted pod cannot initiate a call back into the Harness.
- Each inference job's egress is pinned to its own provider's FQDN by a static per-profile CNP layered on the fail-closed namespace default-deny egress floor. There is no shared-component union egress policy.
- Tool arg values flow to the toolset dispatcher as env vars, never argv; the dispatcher's `"$VAR"` expansion is the only string-to-shell crossing, and the model never authors a shell command.
- Secret values (raw, base64, URL-encoded) are scrubbed from capability-job output before it crosses the gRPC boundary.
- Capability-job pods set `shareProcessNamespace: false`, `automountServiceAccountToken: false`, and a hardened security context (non-root, read-only rootfs, all capabilities dropped).

## Crate Structure

```
crates/
  toolset-proto/       # gRPC proto definitions (toolset.v1)
  toolset-runtime/     # in-toolset execution runtime (tool jobs)
  inference-runtime/   # the inference job binary (the harness's model call)
  model-provider/      # provider dialect parsers (claude, openai, gemini)
```

`toolset-runtime` is the entrypoint of the toolset base image (`images/toolset-base/`): it serves the `ToolJob.Run` stream the Harness dials, receives the validated arg map, execs the dispatcher, and streams frames. Every toolset image builds `FROM` that base and adds only its tools; separate images exist for blast radius, not for the runtime. `inference-runtime` is a standalone scratch-binary image, not a `FROM`-base toolset: the harness dials it over `InferenceJob.Run` to make a remote model call. All provider dialects are bundled in the one image; a call drives exactly the dialect matching the resolved provider format. Per-dialect images would triple the build surface for no security gain, since gVisor and the per-Job secret mount already contain the blast radius.

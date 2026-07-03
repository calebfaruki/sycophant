# Hangar

[![made-with-rust](https://img.shields.io/badge/Made%20with-Rust-1f425f.svg)](https://www.rust-lang.org/)

Kubernetes LLM-dispatch controller for agent workspaces. Turns a fully-assembled message history into an LLM call via the controller + ephemeral Job pattern. Credentials never leave the Job pods. Internal-only: Hangar has no client connection and owns no conversation history.

## How It Works

Two components:

1. **Controller** — k8s controller, one per workspace namespace. Serves gRPC. Watches `Model` and `Provider` CRDs. Creates and manages LLM Jobs. **Stateless** — it holds no conversation store. It receives a complete message history in each `TurnRequest` and returns the result stream.

2. **LLM Job** — stateless Job pod. Connects to the controller via gRPC, pulls a turn assignment (long-poll), reads the API key from a kubelet-mounted Secret, calls the LLM provider, streams the response back. Session-scoped keepalive: the Job loops on `GetTurn` until an idle timeout fires, then exits.

The controller is the only gRPC server. The LLM Jobs and the Transponder connect back to it as clients.

Conversation history lives on the **Transponder**, not Hangar. The Transponder assembles each turn's history, persists user and assistant turns to its per-workspace PVC, and sends Hangar a self-contained `TurnRequest`. See [`docs/transponder.md`](transponder.md).

## Why Hangar

AI agents running in containers need to call LLM APIs, but giving them API keys means:

- **Credential exposure** — a compromised agent leaks your API key
- **No audit trail** — the agent calls whatever it wants with your credentials

Hangar solves this by isolating credentials inside ephemeral Job pods. The controller never sees API keys. It references k8s Secrets by name in Job specs; kubelet mounts them into the pod. The agent runtime (Transponder) knows nothing about keys, models, or providers.

Airlock (`crates/airlock-*`) handles MCP tool isolation. Hangar handles LLM API isolation. Tightbeam (`crates/tightbeam-*`) is the internet-facing client gateway — see [`docs/tightbeam.md`](tightbeam.md).

## Architecture

```
                    gRPC
Transponder ──────────────> Controller
(owns history)                │
                    gRPC      │  creates k8s Jobs
                              │
                         LLM Job
                         (api key
                          mounted)
                              │
                              v
                         OpenRouter API
```

The controller watches CRDs to know which models are available. When a `Turn` arrives carrying the full history, it builds a `TurnAssignment` and enqueues it. The LLM Job pulls it via `GetTurn` (blocking long-poll), calls the LLM, and streams results back via `StreamTurnResult`. The controller forwards events to the Transponder on the `Turn` response stream. It persists nothing.

## CRDs

### Provider

Declares an LLM API endpoint and the credential used to authenticate against it. One Provider can back many Models.

```yaml
apiVersion: sycophant.md/v1
kind: Provider
metadata:
  name: openrouter
  namespace: workspace-my-ws
spec:
  format: openai               # anthropic | openai | gemini
  baseUrl: https://openrouter.ai/api/v1
  secret:
    name: sycophant-llm-openrouter
    # key: api-key             # default; set only if Secret uses a different key
```

### Model

Declares a specific model offered by a provider. The controller creates one LLM Job per model on first use.

```yaml
apiVersion: sycophant.md/v1
kind: Model
metadata:
  name: deepseek-v4-flash
  namespace: workspace-my-ws
spec:
  providerRef:
    name: openrouter
  model: deepseek/deepseek-v4-flash
  params:                       # free-form pass-through, merged into the provider request body
    max_tokens: 8192            # via RFC 7396 JSON Merge Patch. Operator-bound fields
                                # (model, messages, system, tools, stream) are clobbered.
```

The Secret holds one value: the API key. `Provider.spec.secret.key` defaults to `"api-key"` — set it only when the Secret uses a different key name. Kubelet projects the value into the LLM Job. The controller never reads the Secret. See [`docs/secrets-providers.md`](secrets-providers.md) for backend recipes.

## gRPC Protocol

Single service: `hangar.v1.HangarController`. Proto definition at `crates/hangar-proto/proto/hangar/v1/hangar.proto`.

### RPCs

| RPC | Caller | Description |
|-----|--------|-------------|
| `GetTurn` | LLM Job | Long-poll. Blocks until a turn is ready. Job sets gRPC deadline as idle timeout. |
| `StreamTurnResult` | LLM Job | Streams response chunks (content deltas, tool calls) back to the controller. |
| `Turn` | Transponder | Sends a fully-assembled history, receives streaming LLM response events. |

### Turn Flow

1. Transponder assembles the full history and calls `Turn` with it
2. Controller builds a `TurnAssignment` from the request and enqueues it
3. LLM Job's `GetTurn` resolves with the assignment
4. LLM Job calls the LLM provider, streams chunks via `StreamTurnResult`
5. Controller forwards chunks as `TurnEvent`s on the `Turn` response stream
6. Transponder persists the assistant message and decides the next step:
   - If `tool_use`: Transponder executes tools locally, sends results in a new `Turn`
   - If `end_turn` / `max_tokens`: turn complete

The controller persists nothing across turns. Each `TurnRequest` is self-contained.

### Key Types

```protobuf
message TurnRequest {
  optional string system = 1;
  repeated ToolDefinition tools = 2;
  repeated Message messages = 3;
  optional string model = 5;
  optional string reply_channel = 6;
  optional TurnRole role = 7;            // DELEGATE tags per-call delegate turns
  optional string correlation_id = 9;    // orchestrator tool_use_id for delegate scoping
  string conversation_id = 10;           // required; opaque UUID
}

message TurnAssignment {
  optional string system = 1;
  repeated ToolDefinition tools = 2;
  repeated Message messages = 3;
  optional string params_json = 4;       // RFC 7396-merged Model.params
}

message TurnResultChunk {
  oneof chunk {
    ContentDelta content_delta = 1;
    ToolUseStart tool_use_start = 2;
    ToolUseInput tool_use_input = 3;
    TurnComplete complete = 4;
    TurnError error = 5;
    TurnWarning warning = 6;             // a principal param overwritten by an operator-bound field
  }
}
```

`ToolDefinition.parameters_json` and `ToolCall.input_json` are JSON strings, not protobuf `Struct`. `ImageBlock.data` is raw bytes, not base64. The LLM Job handles provider-specific encoding.

`role`/`correlation_id` carry multi-agent semantics: when the orchestrator dispatches a delegate via the `Agent(name, query)` runtime tool, that delegate's `TurnRequest` carries `role: DELEGATE` plus the orchestrator's `tool_use_id` as `correlation_id`. The Transponder uses these to scope each thread's history view; Hangar passes them through.

## Per-Call Model Routing

If a persona file (or `AGENTS.md`) declares a `model:` field in YAML frontmatter, that field selects the model for the turn. The **transponder** parses + strips the frontmatter before dispatch (the LLM never sees the YAML) and sends the resolved model name; hangar looks that name up in the model registry and applies its params into `params_json` for the LLM Job. See [`docs/mainframe.md`](mainframe.md) for the operator/principal-facing convention.

## LLM Job Lifecycle

1. Controller creates a k8s Job referencing the model's Secret by name
2. Kubelet mounts the Secret into the pod
3. Job starts, reads the API key from the mounted file, connects to the controller
4. Job calls `GetTurn` — blocks until work arrives
5. Job calls the LLM provider, streams the response back via `StreamTurnResult`
6. Job loops back to step 4
7. If no work arrives before the gRPC deadline, the Job exits
8. TTL controller cleans up the completed pod after 30 seconds
9. On the next turn, the controller creates a fresh Job if none is connected

The API key exists only in the ephemeral pod's memory and mounted tmpfs. It never appears in gRPC messages, controller memory, or Job spec env vars.

## RBAC

The controller ServiceAccount can create Jobs, read the Model/Provider registry, and authenticate caller SA tokens. It has **zero access to Secrets** — credential Secrets are kubelet-mounted into Job pods and never seen by the controller.

```yaml
rules:
  - apiGroups: ["batch"]
    resources: ["jobs"]
    verbs: ["create", "get", "list", "watch", "delete"]
  - apiGroups: ["sycophant.md"]
    resources: ["models", "providers"]
    verbs: ["get", "list", "watch"]
```

TokenReview is a cluster-scoped API. A shared `cluster-hangar-tokenreview` ClusterRole grants `create` on `tokenreviews`; the `tenant-rolebinding-generator` Kyverno policy binds each tenant's `hangar-ctrl` SA to it. Hangar holds no `clients`/`enrollments`, no `secrets`, and no conversation-store RBAC.

## Security Model

- API keys never appear in gRPC messages or in controller memory
- Credentials only exist in ephemeral Job pods, mounted by kubelet
- Controller RBAC grants no access to Secrets at all
- Job TTL ensures completed pods are cleaned up (30 seconds)
- Each Job mounts exactly one Secret (one credential, one blast radius)
- All images are FROM scratch with musl static builds
- All images signed with cosign (keyless, sigstore)

## Crate Structure

```
crates/
  hangar-providers/      # LLM provider abstraction + shared types
  hangar-proto/          # gRPC proto definitions (hangar.v1)
  hangar-controller/     # k8s controller binary
  hangar-llm-job/        # LLM Job binary
```

## Installation

Container images are published to GHCR on each release:

```
ghcr.io/calebfaruki/hangar-controller:latest
ghcr.io/calebfaruki/hangar-llm-job:latest
```

CRDs (`Model`, `Provider`) ship in the cluster chart (`charts/sycophant-cluster/crds/`) and are installed once per cluster. The per-tenant chart (`charts/sycophant-tenant/`) installs the controller in each workspace namespace. Then create `Model` and `Provider` resources in that namespace.

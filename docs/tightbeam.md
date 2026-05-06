# Tightbeam

[![made-with-rust](https://img.shields.io/badge/Made%20with-Rust-1f425f.svg)](https://www.rust-lang.org/)

Kubernetes communications controller for agent workspaces. Manages LLM calls and channel connections via the controller + Job pattern. Credentials never leave ephemeral Job pods.

## How It Works

Three components:

1. **Controller** -- k8s controller, one per workspace namespace. Serves gRPC. Watches `TightbeamModel` and `TightbeamChannel` CRDs. Creates and manages LLM Jobs and Channel Jobs. Owns conversation history (PVC-backed NDJSON).

2. **LLM Job** -- stateless Job pod. Connects to the controller via gRPC, pulls a turn assignment (long-poll), reads the API key from a kubelet-mounted Secret, calls the LLM provider, streams the response back. Session-scoped keepalive: the Job loops on `GetTurn` until an idle timeout fires, then exits.

3. **Channel Job** -- holds an outbound connection to a messaging platform (Discord, Slack). Bot token mounted by kubelet. Forwards inbound messages to the controller, receives agent responses and sends them to the channel.

The controller is the only gRPC server. Everything else connects back to it as a client.

## Why Tightbeam

AI agents running in containers need to call LLM APIs, but giving them API keys means:

- **Credential exposure** -- a compromised agent leaks your API key
- **No audit trail** -- the agent calls whatever it wants with your credentials
- **No conversation control** -- the agent manages its own context window

Tightbeam solves this by isolating credentials inside ephemeral Job pods. The controller never sees API keys. It references k8s Secrets by name in Job specs; kubelet mounts them into the pod. The agent runtime (Transponder) knows nothing about keys, models, or providers.

Airlock (`crates/airlock-*`) handles MCP tool isolation. Tightbeam handles LLM API isolation.

## Architecture

```
                    gRPC
Transponder ──────────────> Controller ─────> Conversation Log (PVC)
                              │
                    gRPC      │  creates k8s Jobs
              ┌───────────────┤
              │               │
         LLM Job         Channel Job
         (api key          (bot token
          mounted)          mounted)
              │               │
              v               v
         Anthropic API    Discord/Slack
```

The controller watches CRDs to know which models and channels are available. When a turn arrives, it enqueues a `TurnAssignment`. The LLM Job pulls it via `GetTurn` (blocking long-poll), calls the LLM, and streams results back via `StreamTurnResult`. The controller appends the response to conversation history and forwards events to the caller.

## CRDs

### TightbeamProvider

Declares an LLM API endpoint and the credential used to authenticate against it. One TightbeamProvider can back many TightbeamModels.

```yaml
apiVersion: tightbeam.dev/v1
kind: TightbeamProvider
metadata:
  name: anthropic
  namespace: workspace-my-ws
spec:
  format: anthropic            # anthropic | openai | gemini
  baseUrl: https://api.anthropic.com/v1
  secret:
    name: sycophant-llm-anthropic
    # key: api-key             # default; set only if Secret uses a different key
```

### TightbeamModel

Declares a specific model offered by a provider. The controller creates one LLM Job per model on first use.

```yaml
apiVersion: tightbeam.dev/v1
kind: TightbeamModel
metadata:
  name: claude-sonnet
  namespace: workspace-my-ws
spec:
  providerRef:
    name: anthropic
  model: claude-sonnet-4-20250514
  params:                       # free-form pass-through, merged into the provider request body
    max_tokens: 8192            # via RFC 7396 JSON Merge Patch. Operator-bound fields
                                # (model, messages, system, tools, stream) are clobbered.
```

The Secret holds one value: the API key. `TightbeamProvider.spec.secret.key` defaults to `"api-key"` — set it only when the Secret uses a different key name. Kubelet projects the value to `/run/secrets/tightbeam/api-key` inside the LLM Job. The controller never reads the Secret.

### TightbeamChannel

Declares a channel connection. The controller creates Channel Jobs from these.

```yaml
apiVersion: tightbeam.dev/v1
kind: TightbeamChannel
metadata:
  name: discord-bot
  namespace: workspace-my-ws
spec:
  type: discord
  secretName: discord-bot-token
  image: ghcr.io/calebfaruki/tightbeam-channel-discord:latest
```

## gRPC Protocol

Single service: `tightbeam.v1.TightbeamController`. Proto definition at `crates/tightbeam-proto/proto/tightbeam/v1/tightbeam.proto`.

### RPCs

| RPC | Caller | Description |
|-----|--------|-------------|
| `GetTurn` | LLM Job | Long-poll. Blocks until a turn is ready. Job sets gRPC deadline as idle timeout. |
| `StreamTurnResult` | LLM Job | Streams response chunks (content deltas, tool calls) back to the controller. |
| `Turn` | Transponder | Sends messages, receives streaming LLM response events. |
| `ListModels` | Transponder | Returns available models from CRDs. |
| `ChannelStream` | Channel Job | Bidirectional stream. Inbound user messages in, agent responses out. |

### Turn Flow

1. Transponder calls `Turn` with new messages
2. Controller appends messages to conversation history
3. Controller builds `TurnAssignment` from full history and enqueues it
4. LLM Job's `GetTurn` resolves with the assignment
5. LLM Job calls the LLM provider, streams chunks via `StreamTurnResult`
6. Controller forwards chunks as `TurnEvent`s on the `Turn` response stream
7. Controller appends assistant message to conversation log
8. If `tool_use`: transponder executes tools locally, sends results in a new `Turn`
9. If `end_turn` / `max_tokens`: turn complete

### Key Types

```protobuf
message Message {
  string role = 1;
  repeated ContentBlock content = 2;
  repeated ToolCall tool_calls = 3;
  optional string tool_call_id = 4;
  optional bool is_error = 5;
}

message TurnAssignment {
  optional string system = 1;
  repeated ToolDefinition tools = 2;
  repeated Message messages = 3;
}

message TurnResultChunk {
  oneof chunk {
    ContentDelta content_delta = 1;
    ToolUseStart tool_use_start = 2;
    ToolUseInput tool_use_input = 3;
    TurnComplete complete = 4;
    TurnError error = 5;
  }
}
```

`ToolDefinition.parameters_json` and `ToolCall.input_json` are JSON strings, not protobuf `Struct`. `ImageBlock.data` is raw bytes, not base64. The LLM Job handles provider-specific encoding.

## Conversation Ownership

The controller owns the conversation. It persists every message to NDJSON on a PVC. On restart, it rebuilds from the log.

Multi-agent semantics live in the entrypoint, not the runtime. When the orchestrator dispatches a delegate via the workspace's `llm_call` tool, that delegate's `TurnRequest` carries `role: DELEGATE` plus a `correlation_id` (the orchestrator's tool_use id). Delegate-tagged entries are filtered from the orchestrator's `history_for_provider()` view so each thread sees only its own turns. The raw NDJSON retains everything for audit and replay.

Each assistant log entry carries `model` (which TightbeamModel handled the call) and `system_prompt_sha256` (SHA-256 of whatever the orchestrator passed as `system`, including any YAML frontmatter). Auditors compare `sha256sum <persona file>` against log values directly.

Per-call model routing: if a `system_prompt` starts with `---\n…\n---\n` YAML frontmatter, the controller parses it. A `model:` field overrides the inbound `params.model`. The frontmatter is stripped before the body reaches the LLM Job. See [`docs/mainframe.md`](mainframe.md) for the operator/principal-facing convention.

## LLM Job Lifecycle

1. Controller creates a k8s Job referencing the model's Secret by name
2. Kubelet mounts the Secret at `/run/secrets/llm/` inside the pod
3. Job starts, reads API key from the mounted file, connects to controller
4. Job calls `GetTurn` -- blocks until work arrives
5. Job calls LLM provider, streams response back via `StreamTurnResult`
6. Job loops back to step 4
7. If no work arrives before the gRPC deadline, Job exits
8. TTL controller cleans up the completed pod after 30 seconds
9. On next turn, controller creates a fresh Job if none is connected

The API key exists only in the ephemeral pod's memory and mounted tmpfs. It never appears in gRPC messages, controller memory, or Job spec env vars.

## LLM Secret Format

The k8s Secret referenced by `TightbeamModel.spec.secretName` must contain these keys:

```
provider     -> "anthropic"
model        -> "claude-sonnet-4-20250514"
api-key      -> "sk-ant-..."
max-tokens   -> "8192"          # optional, defaults to 8192
```

Values are trimmed of whitespace. Missing `provider`, `model`, or `api-key` is a hard error.

## RBAC

The controller ServiceAccount has zero Secret read access:

```yaml
rules:
  - apiGroups: ["batch"]
    resources: ["jobs"]
    verbs: ["create", "get", "list", "watch", "delete"]
  - apiGroups: ["tightbeam.dev"]
    resources: ["tightbeammodels", "tightbeamchannels"]
    verbs: ["get", "list", "watch"]
```

Secrets are referenced by name in Job specs. Kubelet handles the mount. The controller never touches credential bytes.

## Security Model

- API keys and bot tokens never appear in gRPC messages
- API keys and bot tokens never appear in controller memory
- Credentials only exist in ephemeral Job pods, mounted by kubelet
- Controller RBAC has zero Secret read access
- Job TTL ensures completed pods are cleaned up (30 seconds)
- Each Job mounts exactly one Secret (one credential, one blast radius)
- All images are FROM scratch with musl static builds
- All images signed with cosign (keyless, sigstore)

## Crate Structure

```
crates/
  tightbeam-providers/      # LLM provider abstraction + shared types
  tightbeam-proto/          # gRPC proto definitions (tightbeam.v1)
  tightbeam-controller/     # k8s controller binary
  tightbeam-llm-job/        # LLM Job binary
```

## Installation

Container images are published to GHCR on each release:

```
ghcr.io/calebfaruki/tightbeam-controller:latest
ghcr.io/calebfaruki/tightbeam-llm-job:latest
```

Install via the sycophant Helm chart (`charts/sycophant/`); CRDs (`TightbeamChannel`, `TightbeamModel`) ship as templates and are applied automatically by `helm install` / `helm upgrade`. Then create `TightbeamModel` and `TightbeamChannel` resources in the workspace namespace.

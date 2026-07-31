# CLAUDE.md — Airlock Project Guide

This file is the source of truth for the airlock project. Every Claude Code session must read and follow this document. Do not deviate from the architecture, naming, file paths, or design decisions described here without explicit approval.

## What is Airlock?

Airlock is a Kubernetes tool execution controller. It watches Chamber CRDs, discovers tools from OCI image labels, serves gRPC to transponder, and creates ephemeral Jobs for each tool call. Each tool declares a structured argument schema in the image LABEL; the controller validates the LLM's input against that schema, then spawns a Job that execs the chamber's own dispatcher (`/etc/chamber/dispatch`) with the tool name as argv[1] and arg values as env vars. The dispatcher is chamber-author code — a shell case-statement, a Makefile wrapper, a native binary — whatever fits the underlying CLI.

The controller never reads Secrets — kubelet mounts credentials into Jobs. Containers never hold credentials beyond the lifetime of a single Job. The LLM never authors a shell command; airlock validates arg values against the declared schema and the chamber's dispatcher is the only place where any string-to-shell crossing happens (with `"$VAR"` quoting, single-token argv).

## Architecture

Three components:

1. **airlock-controller** — k8s controller binary. Watches Chamber CRDs. Discovers tools from OCI image labels (`md.sycophant.tools`). Serves gRPC (WatchTools, CallTool, GetToolCall, SendToolResult). Validates LLM input against each tool's declared arg schema. Creates ephemeral Jobs per tool call. One per namespace.
2. **airlock-runtime** — chamber runtime binary included in every tool Job image. Connects back to the controller via gRPC. Receives the tool name + validated arg map. Execs `/etc/chamber/dispatch <tool_name>` with arg values as env vars. Returns stdout/stderr/exit code.
3. **airlock-proto** — gRPC service and message definitions. Package namespace: `airlock.v1`.

### Controller-as-Server Pattern

The controller is the only gRPC server. Tool Jobs connect back to the controller as clients. The controller creates a Job with its own address as an env var (`AIRLOCK_CONTROLLER_ADDR`). The Job starts, connects, pulls work, executes, returns results. This eliminates Job endpoint discovery.

## Protocol

gRPC over HTTP/2. Service: `airlock.v1.AirlockController`.

| RPC | Direction | Purpose |
|-----|-----------|---------|
| `ListTools` | transponder → controller | List available tools discovered from chamber images |
| `CallTool` | transponder → controller | Execute a tool (blocks until Job completes) |
| `GetToolCall` | runtime → controller | Pull work assignment (long-poll) |
| `SendToolResult` | runtime → controller | Return execution result |

Proto definition: `crates/airlock-proto/proto/airlock/v1/airlock.proto`

## Tool Discovery

Tools are discovered from OCI image labels. When a Chamber has an `image` field, the controller fetches the image config from the registry and reads the `md.sycophant.tools` label.

### Label Format

A JSON array of tool declarations. Each entry is an object with `name`, optional `description`, and required `args` (which may be an empty object for zero-arg tools).

```json
[
  {
    "name": "git-clone",
    "description": "Clone a git repository.",
    "args": {
      "url":  {"type": "string", "required": true, "env": "URL",  "description": "Repository URL."},
      "dest": {"type": "string", "required": true, "env": "DEST", "description": "Destination directory."}
    }
  },
  {
    "name": "git-status",
    "description": "Show working tree status.",
    "args": {}
  }
]
```

Per-arg fields:
- `type` (required) — one of `string`, `integer`, `number`, `boolean`.
- `required` (default `false`) — whether the LLM must provide a value.
- `env` (required) — the environment variable name the runtime sets when invoking the chamber's dispatcher.
- `description` (optional) — surfaced to the LLM in the schema.

Missing label = no tools (not an error). Malformed entries reject the whole label with a clear error message; there is no partial parsing.

### Tool Naming Convention

Tool names become K8s resource names at dispatch time (`airlock-{tool}-{call_id_prefix}` is the per-call Job name). They must be valid RFC 1123 subdomain components — lowercase alphanumeric and hyphens only, starting and ending with an alphanumeric character, length 1–63.

**Use kebab-case** (`notion-search`, `git-status`, `ssh-exec`). **Do not use** snake_case (`notion_search`), camelCase (`notionSearch`), or any other form. The controller validates names at chamber discovery and refuses to register a chamber whose label declares a non-conforming name — you'll see a precise error in the `airlock-controller` logs naming the offending chamber + tool, with a kebab-case suggestion. This trades a small one-time naming constraint for catching the problem at deploy time instead of at the first tool call (where K8s would reject the Job creation with a confusing 422).

If you're porting an existing chamber that uses snake_case, rename the tool in three places: the Dockerfile's `md.sycophant.tools` LABEL, the dispatch script's case branches (or Makefile targets), and any documentation referencing the tool name.

### Duplicate Tool Names

If two chambers declare the same tool name, the first chamber wins. The second is rejected with a warning log. No silent override.

## Chamber CRD

```yaml
apiVersion: sycophant.md/v1
kind: Chamber
metadata:
  name: git-ops
spec:
  image: ghcr.io/calebfaruki/airlock-git:latest
  credentials:
    - secret: git-ssh-key
      file: /root/.ssh/id_ed25519
  egress:
    - host: github.com
      port: 22
  keepalive: false
```

### CRD Fields

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `image` | string | optional | OCI image with `md.sycophant.tools` label. Tools discovered from this image. |
| `credentials` | array | `[]` | Credential mappings (env or file mode) |
| `egress` | array | `[]` | Allowed egress rules (`domain` + `port`). Trust unit is the registrable domain — `domain: notion.com` covers `notion.com` + `*.notion.com`. |
| `keepalive` | bool | `false` | Keep the Job alive for multiple calls |

The workspace PVC is always mounted RW at `/workspace` — convention, not configurable.

## Command Execution

The LLM sends a tool call with structured input matching the tool's declared schema, e.g. `{"url": "git@github.com:foo/bar", "dest": "bar"}` for `git_clone`. The controller validates this input against the tool's `args` declaration (required keys present, types match, no unknown keys) and constructs a map of `env_name -> value`. The runtime receives that map in the `ToolCallAssignment` and execs `/etc/chamber/dispatch <tool_name>` with each value as an env var. No shell parsing of LLM input ever occurs — values flow `LLM JSON → controller HashMap → OS process env` and emerge in the chamber's dispatcher as native env vars.

### Tool Parameter Schema

Each tool's parameter schema is synthesized from its `args` declaration by `airlock-controller::validation::synthesize_schema`. For example, the `git_clone` tool above produces:

```json
{
  "type": "object",
  "properties": {
    "url":  {"type": "string", "description": "Repository URL."},
    "dest": {"type": "string", "description": "Destination directory."}
  },
  "required": ["url", "dest"]
}
```

### Security Boundary

- **The LLM never authors a shell command.** The controller validates structured input against the tool's schema; runtime passes values via env vars.
- **Args flow through env vars, never argv.** Putting a `KEY=val` argv into `make` would let make's `$(VAR)` expansion smuggle the value into recipe text before the shell parses it. The runtime only ever passes env vars.
- **Dispatcher is the only shell crossing.** Chamber recipes use `"$VAR"` (double-quoted env-var expansion), which is a single argv token regardless of contents.
- **Output scrubbing** — secret values (raw, base64, URL-encoded) are redacted from stdout/stderr before crossing the gRPC boundary.
- **Defense in depth**: the Job has no credentials beyond what's explicitly mounted via the chamber's credential spec.

## Job Lifecycle

- **Fire-and-forget** (keepalive=false): new Job per CallTool. Runtime runs one command, exits. TTL cleanup (30s).
- **Keepalive** (keepalive=true): one Job persists, runtime loops on GetToolCall. Controller tracks idle time. Job deleted after idle timeout.

## RBAC

The controller ServiceAccount has zero Secret read access:

```yaml
rules:
  - apiGroups: ["batch"]
    resources: ["jobs"]
    verbs: ["create", "get", "list", "watch", "delete"]
  - apiGroups: ["sycophant.md"]
    resources: ["chambers"]
    verbs: ["get", "list", "watch"]
  - apiGroups: ["sycophant.md"]
    resources: ["chambers/status"]
    verbs: ["patch"]
```

Credentials are referenced by name in Job specs. Kubelet mounts them. The controller never touches credential bytes.

## Directory Layout

```
crates/
  airlock-proto/              # gRPC proto definitions
    proto/airlock/v1/airlock.proto
    build.rs
    src/lib.rs
  airlock-controller/         # k8s controller binary
    src/main.rs               # CLI + tokio runtime
    src/crd.rs                # Chamber CRD struct
    src/state.rs              # shared controller state (RegisteredTool, chambers, call queue)
    src/registry.rs           # OCI registry client for reading image labels
    src/watcher.rs            # kube-rs CRD watcher with tool discovery
    src/grpc.rs               # gRPC service implementation
    src/job.rs                # k8s Job builder
    src/keepalive.rs          # background cleanup task
  airlock-runtime/            # chamber runtime binary
    src/main.rs               # gRPC client loop
    src/execute.rs            # compose_dispatch_command + run_dispatch
    src/scrub.rs              # output scrubbing (secret redaction)
images/
  git/Dockerfile              # built-in git tool image (LABEL md.sycophant.tools=[...])
  git/dispatch                # chamber dispatcher (shell case-statement)
charts/sycophant-cluster/
  crds/chamber.yaml                            # Chamber CRD (installed once per cluster)
charts/sycophant-tenant/
  templates/airlock-ctrl.yaml                  # controller Deployment
  templates/airlock-ctrl-rbac.yaml             # controller RBAC
  templates/airlock-ctrl-netpol.yaml           # controller CiliumNetworkPolicy
  templates/airlock-job-baseline-netpol.yaml   # fail-closed chamber egress baseline
```

## Distribution

Container images published to GHCR:
- `ghcr.io/calebfaruki/airlock-controller:latest` — distroless/cc base (glibc for kube-rs TLS)
- `ghcr.io/calebfaruki/airlock-runtime:latest` — scratch base (static musl)
- `ghcr.io/calebfaruki/airlock-git:latest` — alpine + git + airlock-runtime

Release artifacts: `airlock-controller-linux-{amd64,arm64}`, `airlock-runtime-linux-{amd64,arm64}`

All artifacts signed with cosign. Build provenance attestations via SLSA.

## Security Invariants

These must never be violated:

1. **Credentials never appear in gRPC messages.** No tokens, no keys, no secret bytes in transit.
2. **Credentials never appear in controller memory.** The controller references Secrets by name only.
3. **Controller RBAC has zero Secret read access.** Kubelet mounts credentials into Jobs.
4. **Chamber Jobs exec a fixed dispatcher path.** Runtime spawns `/etc/chamber/dispatch <tool_name>` with arg values as env vars. The dispatcher is the only string-to-shell boundary and is chamber-author code; LLM input never enters argv.
5. **shareProcessNamespace is false on all Job pods.** Prevents cross-container `/proc` access.
6. **Job TTL ensures cleanup.** Completed Jobs are garbage-collected (30s default).
7. **Secret values are scrubbed from command output before crossing the gRPC boundary.** The runtime redacts raw, base64-encoded, and URL-encoded secret values from stdout/stderr before sending results to the controller.
8. **All images are signed with cosign.** Keyless, sigstore-backed.

## What Airlock Is NOT

- **Not a workflow engine.** It executes tool calls. Approval flows belong to the agent framework.
- **Not an MCP server.** Each tool dispatches to a chamber-provided executable; no protocol translation.
- **Not a framework.** It is a single-purpose controller. No opinions on agent architecture.
- **Not a timeout manager.** Commands run until they finish.

## Lints

Workspace-wide lint configuration lives in the root `Cargo.toml` under `[workspace.lints]`. Individual crates inherit via `[lints] workspace = true`. Do not add lint attributes (`#![deny(...)]`, `#![warn(...)]`) in source files.

After any refactor, run `cargo clippy --workspace` and fix all warnings before committing. Dead code, unused imports, and unused variables are denied (compile errors, not warnings).

Proto-generated code is output to `OUT_DIR` and wrapped with `#[allow(clippy::all, unreachable_pub)]` in `airlock-proto/src/lib.rs`.

## Build Requirements

- Rust 1.94.0+ (stable)
- `protoc` (protobuf compiler) for proto code generation
- On macOS: `brew install protobuf`
- In CI: `arduino/setup-protoc` action

## External Systems

- **transponder**: calls ListTools/CallTool on the controller. No transponder code in this repo.
- **relay**: referenced architecture pattern (controller-as-server). No code dependency.
- **sycophant**: Job label `app.kubernetes.io/part-of=sycophant`. Organizational label only.

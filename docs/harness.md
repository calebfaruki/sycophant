# Harness

The Harness is the agent runtime, one per workspace. It runs the agent loop, owns the conversation history, and reads its own instructions in-process. There is one harness Deployment per workspace; each serves only its own workspace.

## Instructions serving

The instructions are the principal-authored content that drives agent behavior — most importantly the `AGENTS.md` that becomes the agent's system prompt. The harness reads it **in-process** from a read-only volume it mounts; there is no separate instructions-serving pod and no instructions RPC.

The instructions form a read-only, entrypoint-driven principal source. Content is synced at pod start by an init container from this workspace's object-storage prefix into an emptyDir the serving container mounts read-only. The read-only, prefix-scoped sync credential is mounted into the init container only; the serving container holds no credential.

### Layout conventions

The instructions are the principal's OS. Real OSes have non-configurable layouts (`/etc`, `/var`, `/usr`); programs that respect them just work. Sycophant follows the same principle: structure is conventional, the source path is configurable. If every principal would pick the same answer, the chart doesn't ask.

Each workspace's harness mounts its own instructions read-only at `/etc/instructions/<workspace>/`. That directory is the workspace's whole instruction set.

Layout inside `/etc/instructions/<workspace>/`:

- `AGENTS.md` — the agent's system prompt source. The harness reads it in-process and passes the contents as the system prompt for every model call. Aligns with the [Linux Foundation Agentic AI Foundation's AGENTS.md convention](https://agents.md/).
- `agents/<name>.md` — per-delegate agent for orchestrator-style agents. Dispatched via the `dispatch(path, query)` runtime tool, which reads the file at `path` from the mounted instructions and runs a delegate sub-turn on the file's frontmatter model and tools. The convention is recursive: each delegate is a sub-agent rooted at its own agent file.
- `skills/<name>.md` — free-form markdown describing how to perform a focused task. The harness surfaces the whole tree to the LLM through generic path-based runtime tools (`read(path)`, `list()`), sourced from this directory — the agent lists and reads them on demand rather than from a filesystem path. Lets the principal build a library of how-to-do-X documents that don't bloat the system prompt.
- `<topic>/` — free-form subdirectories for anything else (project context, glossaries, FAQs). The root AGENTS.md points at what's relevant.

Sycophant's interpretation of AGENTS.md is "the agent's file at this level of the OS." The canonical AGENTS.md spec is silent on agent content (it scopes itself to project context); using it recursively for delegate agents extends the convention rather than contradicting it.

Trust contract:

- The cluster never writes to the instructions. All writes happen at the source, controlled by the principal, who uploads the instruction tree to the workspace's object-storage prefix out-of-band. The init container's sync is read-only (GET/LIST), prefix-scoped to this workspace.
- Each workspace has its **own** instructions — different AGENTS.md, different skills, different sub-agents. Multiple workspaces in the same namespace are *different agents*, not copies of one. A harness syncs only its own workspace's prefix, so it can never read another workspace's content. The serving container holds no Kubernetes API grant, no Secret access, and no `jobs` or `instructions` RBAC; reading the instructions is a local filesystem read. Only the init container mounts the read-only, prefix-scoped sync credential.

### How it's wired

Instructions content is chart-value driven, not a custom resource. The chart renders, per workspace from `.Values.workspaces` (no `lookup`), an `emptyDir` mounted read-only onto that workspace's harness at `/etc/instructions/<workspace>` plus an init container that fills it.

For each workspace the harness pod runs an `instructions-sync` init container that pulls `<harness.instructions.bucket>/<workspace prefix>` from `<harness.instructions.endpoint>` into the `emptyDir`, using the read-only credential named by `harness.instructions.credentialName` (a SealedSecret produced SaaS-side). The workspace prefix is `workspaces.<ws>.instructions.prefix`, defaulting to `<namespace>/<workspace>`. The init container mounts the credential; the serving container does not. `emptyDir` (not `hostPath`, not a PV) keeps the tenant namespace PSA `restricted` and re-syncs a fresh tree on every pod start.

```
<bucket>/<prefix>  →  init sync  →  emptyDir /etc/instructions/<ws>  →  agent
```

Because delivery renders per-workspace from values with no `lookup`, the chart renders identically under `helm install` and a GitOps `helm template | kubectl apply` pipeline — neither strips a instructions.

**MinIO ingress lock.** The object store is a shared cluster component (`charts/sycophant-objectstore`, installed once in `sycophant-system`), so it owns its own network posture. Its `objectstore` CiliumNetworkPolicy admits only harness pods (`app.kubernetes.io/component: harness`), in any tenant namespace via a `k8s:io.kubernetes.pod.namespace` Exists match, to the store's API port, and denies every other pod. It selects the store pod by `app.kubernetes.io/component: objectstorage` + `app.kubernetes.io/part-of: sycophant`, and its egress is locked to cluster DNS only (no external egress). The tenant chart's harness egress policy is the tenant-side half: it lets the init container reach the store cross-namespace by component label plus the store namespace read from `harness.instructions.endpoint`, and adds that host to the DNS L7 allowlist.

### Authoring instructions

Instructions are content, not a custom resource — author them per workspace by uploading the agent files to the workspace's object-storage prefix. With no custom prefix, content lives at the convention key `<namespace>/<workspace>` in `harness.instructions.bucket`. Upload the agent files there and the next pod start syncs them to `/etc/instructions/<workspace>`. To override the source for one workspace, set that workspace's `instructions.prefix` in `.Values.workspaces` to a custom bucket prefix.

### ValidatingAdmissionPolicy on hostPath

The `cluster-gvisor-pod-policy` VAP forbids `hostPath` volumes on **all** sycophant pods — there is no per-pod exception. Instructions content is delivered through an `emptyDir` filled by the init-container sync (no `hostPath`, no PV), so no pod needs one.

### Subsystem-level config

The top-level `harness:` block holds operator-level settings, including the object-storage delivery contract:

```yaml
harness:
  image: sycophant-harness
  tag: local
  pullPolicy: Never
  instructions:
    endpoint: minio.sycophant-system.svc.cluster.local:9000
    bucket: sycophant-instructions
    credentialName: instructions-reader
    syncImage: quay.io/minio/mc@sha256:a7fe349ef4bd8521fb8497f55c6042871b2ae640607cf99d9bede5e9bdf11727
```

## Reference fixtures

[`examples/instructions/`](../examples/instructions/) holds a fixture you can copy onto the host path as a starting point:

- [`simple/`](../examples/instructions/simple/) — minimal assistant with local tools only. Single `AGENTS.md`.

## Routing delegates to specific models

An agent file (or `AGENTS.md` itself) MAY declare a `model:` field in YAML frontmatter at the top of the file. The **harness** owns the whole path:

1. It parses the frontmatter (delimited by `---` lines, max 4 KiB), selects the `model:` name (or resolves `inherit` from the conversation log), and strips the frontmatter from the system prompt before dispatch — the LLM never sees the YAML.
2. It resolves the name against its own catalog — a key of the operator-declared `model` configuration — and dials that destination directly: a warm in-cluster inference Service in-process, or a per-call `inference-runtime` Job the harness creates and dials. A name with no matching entry is refused, never defaulted.

Example. With two models (`fast` and `smart`):

```yaml
model:
  fast:
    image: ghcr.io/calebfaruki/inference-runtime:latest
    format: openai
    model: deepseek/deepseek-v4-flash
    baseUrl: https://openrouter.ai/api/v1
    secret: sycophant-llm-openrouter
  smart:
    image: ghcr.io/calebfaruki/inference-runtime:latest
    format: openai
    model: deepseek/deepseek-r1
    baseUrl: https://openrouter.ai/api/v1
    secret: sycophant-llm-openrouter
```

Agent files declare which to use:

```markdown
---
model: smart
---
You are Alice. You are warm and creative...
```

```markdown
---
model: fast
---
You are Bob. You are dry and technical...
```

Files without frontmatter dispatch to whichever model the request specified. If neither names one, the turn is refused with a `failed_precondition` error. There is no fallback and no reserved default name.

**Audit story.** The `system_prompt_sha256` field on each assistant log entry is computed on the **pre-strip** value — i.e., the verbatim file contents the orchestrator passed. External auditors run `sha256sum agents/alice.md` on the canonical file and the value matches the log directly. No frontmatter-stripping step needed in the audit tooling.

**Failure mode.** If `model:` references a name with no declared model entry, the call fails fast with a `failed_precondition` error naming the missing model. Operators discover available names under `model` in the chart's values.

## Narrowing advertised tools per agent

An agent file MAY declare a `tools:` list in the same YAML frontmatter as `model:`:

```markdown
---
model: fast
tools: [Shell, Read, Write]
---
You are Bob...
```

The list narrows the toolset tools advertised to the model for that agent's turns. It is model-ergonomics, not an authorization boundary: a light local model tool-calls more reliably against a short list than the full catalog.

- It scopes only bound toolset tools. Runtime tools (`read`, `dispatch`, `list`, `Think`, and siblings) and channel tools (`RevealPath`, `RequestUserInput`, `RequestUserAuth`) are always advertised, regardless of the list.
- It cannot widen access. The server-side authorization gate keys on the workspace and toolset binding alone and is agent-blind. A tool the agent omits stays executable if the model names it; a tool the agent lists but the workspace lacks stays denied.
- An entry matching no known tool, or a list that excludes every bound toolset tool, is warn-logged and non-fatal. The turn still advertises the runtime and channel tools.
- An absent `tools:` key advertises the full router snapshot, identical to prior behavior.

## Future work

- **Remote-source instructions adapters** — OCI, lakeFS, and git adapters beyond the object-storage sync ship as separate-repo crates with their own controllers. Each populates the workspace's object-storage prefix out-of-band; the serving container reads only its synced emptyDir.
- **CLI helpers** — `syco init` to scaffold a new instructions folder.
- **Web UI / SaaS authoring surface** — operator-facing app for editing principal content (Rails admin).

## Verification

After install, inspect the instructions from the workspace's harness pod:

```bash
kubectl exec -n <ns> deploy/<workspace> -c harness -- ls -la /etc/instructions/<workspace>
kubectl exec -n <ns> deploy/<workspace> -c harness -- cat /etc/instructions/<workspace>/AGENTS.md
```

The instructions files should be present and readable.

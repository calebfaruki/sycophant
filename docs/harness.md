# Harness

The Harness is the agent runtime, one per workspace. It runs the agent loop, owns the conversation history, and reads its own kernel in-process. There is one harness Deployment per workspace; each serves only its own workspace.

## Kernel serving

The kernel is the principal-authored content that drives agent behavior — most importantly the `AGENTS.md` that becomes the agent's system prompt. The harness reads it **in-process** from a read-only volume it mounts; there is no separate kernel-serving pod and no kernel RPC.

The kernel is a read-only, entrypoint-driven principal source. Content is delivered on a single operator-populated read-only volume; the framework fetches nothing from any remote source.

### Layout conventions

The kernel is the principal's OS. Real OSes have non-configurable layouts (`/etc`, `/var`, `/usr`); programs that respect them just work. Sycophant follows the same principle: structure is conventional, the source path is configurable. If every principal would pick the same answer, the chart doesn't ask.

Each workspace's harness mounts its own kernel read-only at `/etc/kernels/<workspace>/`. That directory is the whole kernel for the workspace.

Layout inside `/etc/kernels/<workspace>/`:

- `AGENTS.md` — the agent's system prompt source. The harness reads it in-process and passes the contents as the system prompt for every model call. Aligns with the [Linux Foundation Agentic AI Foundation's AGENTS.md convention](https://agents.md/).
- `agents/<name>.md` — per-delegate persona for orchestrator-style agents. Loaded via the `Agent(name, query)` runtime tool, which reads `agents/<name>.md` from the mounted kernel and dispatches a delegate sub-conversation. The convention is recursive: each delegate is a sub-agent rooted at its own persona file.
- `skills/<name>.md` — free-form markdown describing how to perform a focused task. The harness surfaces skills to the LLM as read-only **kernel tools** (list and read), sourced from this directory — the agent lists and reads them on demand rather than from a filesystem path. Lets the principal build a library of how-to-do-X documents that don't bloat the system prompt.
- `<topic>/` — free-form subdirectories for anything else (project context, glossaries, FAQs). The root AGENTS.md points at what's relevant.

Sycophant's interpretation of AGENTS.md is "the agent's file at this level of the OS." The canonical AGENTS.md spec is silent on persona content (it scopes itself to project context); using it recursively for delegate personas extends the convention rather than contradicting it.

Trust contract:

- The cluster never writes to the kernel. All writes happen at the source, controlled by the principal. The operator populates the read-only volume out-of-band (a direct edit on the host filesystem, `aws s3 cp`, rsync, or a CI step).
- Each workspace has its **own** kernel — different AGENTS.md, different skills, different sub-agents. Multiple workspaces in the same namespace are *different agents*, not copies of one. A harness mounts only its own workspace's kernel PVC, so it can never read another workspace's content. The harness holds no Kubernetes API grant, no Secret access, and no `jobs` or `kernels` RBAC; reading the kernel is a local filesystem read.

### How it's wired

Kernel content is chart-value driven, not a custom resource. The chart renders **one PV per workspace** from `.Values.workspaces` (no `lookup`), each mounted read-only onto that workspace's harness at `/etc/kernels/<workspace>`.

For each workspace the chart renders one cluster-scoped read-only `PersistentVolume` `kernel-<workspace>-<namespace>` whose `hostPath` is `<hostPathBase>/<namespace>/<workspace>` (or the workspace's custom `kernel.path`), `type: DirectoryOrCreate`, plus a namespaced `ReadOnlyMany` PVC `kernel-<workspace>` that the harness mounts read-only at `/etc/kernels/<workspace>`. A custom `kernel.path` is simply that workspace's serving-PV `hostPath` — no separate "override" resource. PSA `restricted` forbids pod `hostPath` volumes but allows PVCs and never inspects the cluster-scoped PV — so the tenant namespace stays `restricted` while preserving local live-edit. The node sees the base via the `syco setup` bind-mount (`syco tenant up` sets `hostPathBase`); GitOps operators set their own node path.

```
<base>/<ns>/<ws>  →  harness /etc/kernels/<ws>  →  agent
```

Because delivery renders per-workspace from values with no `lookup`, the chart renders identically under `helm install` and a GitOps `helm template | kubectl apply` pipeline — neither strips a kernel.

The mount *is* the host filesystem, not a copy: edits from outside the cluster (in the operator's editor) appear inside the harness on the next `read(2)`.

### Authoring a kernel

Kernel is content, not a custom resource — author it per workspace by dropping the persona files on the read-only volume. With no custom path, content lives at the convention location `<hostPathBase>/<namespace>/<workspace>` (the CLI's bind-mounted kernels dir locally). Drop the persona files there and they appear live at `/etc/kernels/<workspace>`. To override the source for one workspace, set that workspace's `kernel.path` in `.Values.workspaces` to a custom host directory; `syco tenant up` passes it through as a per-workspace helm value. On local k3d the custom dir must live under the bind-mounted `~/.config/sycophant/kernels` tree to be visible in the node; on a real cluster it must exist on the node the pod schedules to.

### ValidatingAdmissionPolicy on hostPath

The `cluster-gvisor-pod-policy` VAP forbids `hostPath` volumes on **all** sycophant pods — there is no per-pod exception. Local kernels are delivered through a PVC bound to a cluster-scoped PV (the `hostPath` lives on the PV, never on a pod), so no pod needs one.

### Subsystem-level config

The top-level `harness:` block holds operator-level settings, including the node-side kernel root:

```yaml
harness:
  image: sycophant-harness
  tag: local
  pullPolicy: Never
  kernels:
    hostPathBase: /var/lib/sycophant/kernels
```

## Reference fixtures

[`examples/mainframe/`](../examples/mainframe/) holds a fixture you can copy onto the host path as a starting point:

- [`simple/`](../examples/mainframe/simple/) — minimal assistant with local tools only. Single `AGENTS.md`.

## Routing delegates to specific models

A persona file (or `AGENTS.md` itself) MAY declare a `model:` field in YAML frontmatter at the top of the file. Ownership splits across the harness and the toolset controller:

1. The **harness** parses the frontmatter (delimited by `---` lines, max 4 KiB), selects the `model:` name (or resolves `inherit` from the conversation log), and strips the frontmatter from the system prompt before dispatch — the LLM never sees the YAML.
2. It sends the resolved model name + stripped system + assembled history to the **toolset controller**, which looks the name up as a profile key of the operator-declared `prompt` toolset and dispatches the call to that profile's prompt worker. A name with no profile is refused, never defaulted.

Example. With two profiles (`fast` and `smart`):

```yaml
toolsets:
  prompt:
    image: prompt-toolset
    profiles:
      fast:
        TOOLSET_MODEL: deepseek/deepseek-v4-flash
      smart:
        TOOLSET_MODEL: deepseek/deepseek-r1
```

Persona files declare which to use:

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

**Failure mode.** If `model:` references a name with no profile, the call fails fast with a `failed_precondition` error naming the missing model. Operators discover available names under `toolsets.prompt.profiles` in the chart's values.

## Future work

- **Remote-source kernel adapters** — OCI, lakeFS, git, and S3 adapters ship as separate-repo crates with their own controllers. Each populates the read-only serving volume out-of-band; the framework itself fetches nothing.
- **CLI helpers** — `syco init` to scaffold a new kernel folder.
- **Web UI / SaaS authoring surface** — operator-facing app for editing principal content (Rails admin).

## Verification

After install, inspect the kernel from the workspace's harness pod:

```bash
kubectl exec -n <ns> deploy/<workspace> -c harness -- ls -la /etc/kernels/<workspace>
kubectl exec -n <ns> deploy/<workspace> -c harness -- cat /etc/kernels/<workspace>/AGENTS.md
```

The kernel files should be present and readable.

# Mainframe

The Mainframe is the `mainframe-ctrl` controller pod. It holds the principal-authored files that drive agent behavior — most importantly the `AGENTS.md` that becomes the agent's system prompt — and serves them to each workspace's transponder over gRPC. The transponder does **not** mount the kernel; it fetches content per turn via the `GetAgent` RPC.

The kernel is a read-only, entrypoint-driven principal source with a pluggable source-kind discriminator; both `kind: HostPath` and `kind: S3` ship in-tree.

## Layout conventions

The mainframe is the principal's OS. Real OSes have non-configurable layouts (`/etc`, `/var`, `/usr`); programs that respect them just work. Sycophant's mainframe follows the same principle: structure is conventional, the source path is configurable. If every principal would pick the same answer, the chart doesn't ask.

The kernel for each workspace lives at `/etc/kernels/<namespace>/<workspace>/` **inside the `mainframe-ctrl` pod** (one subdirectory per workspace). The transponder reaches it only through RPC, never a shared mount.

Layout inside `/etc/kernels/<namespace>/<workspace>/`:

- `AGENTS.md` — the agent's system prompt source. The transponder calls `GetAgent("")` on every turn (served from a short-lived persona cache) and passes the contents as the system prompt for every Hangar call. Aligns with the [Linux Foundation Agentic AI Foundation's AGENTS.md convention](https://agents.md/).
- `agents/<name>.md` — per-delegate persona for orchestrator-style agents. Loaded via the `Agent(name, query)` runtime tool, which calls `GetAgent(name)` (returning `agents/<name>.md`) and dispatches a delegate sub-conversation. (Earlier versions used a chamber-side `llm_call` tool; the current path is a runtime tool backed by the mainframe RPC.) The convention is recursive: each delegate is a sub-agent rooted at its own persona file.
- `skills/<name>.md` — free-form markdown describing how to perform a focused task. The transponder surfaces skills to the LLM as read-only **mainframe tools** (list and read), sourced from this directory — the agent lists and reads them on demand rather than from a filesystem path. Lets the principal build a library of how-to-do-X documents that don't bloat the system prompt.
- `<topic>/` — free-form subdirectories for anything else (project context, glossaries, FAQs). The root AGENTS.md points at what's relevant.

Sycophant's interpretation of AGENTS.md is "the agent's file at this level of the OS." The canonical AGENTS.md spec is silent on persona content (it scopes itself to project context); using it recursively for delegate personas extends the convention rather than contradicting it.

Trust contract:

- The cluster never writes to the Mainframe. All writes happen at the source, controlled by the principal — directly on the host filesystem (`HostPath`) or in the source bucket (`S3`).
- Each workspace has its **own** kernel — different AGENTS.md, different skills, different sub-agents. Multiple workspaces in the same namespace are *different agents*, not copies of one. Per-workspace reads are scoped by the transponder's single-audience SA token (`transponder.mainframe.sycophant.md`), which `mainframe-ctrl` verifies before rooting any read at that workspace's directory.

## How it's wired

Each workspace's kernel is a `Kernel` CR — a discriminated source (`kind: HostPath` or `kind: S3`) whose **name is the workspace name**. Author it with `syco tenant kernel set` (or `kubectl apply` a `Kernel` CR). Delivery does **not** read the CRs at chart-render time: `syco tenant up` reads each Kernel CR and passes its kind (+ optional custom path) as a per-workspace helm value; the chart renders **one PV per workspace** from `.Values.workspaces` (no `lookup`), each mounted at `/etc/kernels/<namespace>/<workspace>`.

- **`HostPath`** — for each workspace the chart renders one cluster-scoped read-only `PersistentVolume` `kernel-<workspace>-<namespace>` whose `hostPath` is `<hostPathBase>/<namespace>/<workspace>` (or the workspace's custom `--path`), `type: DirectoryOrCreate`, plus a namespaced `ReadOnlyMany` PVC `kernel-<workspace>` that `mainframe-ctrl` mounts read-only at `/etc/kernels/<namespace>/<workspace>`. A custom `--path` is simply that workspace's serving-PV `hostPath` — no separate "override" resource. PSA `restricted` forbids pod `hostPath` volumes but allows PVCs and never inspects the cluster-scoped PV — so the tenant namespace stays `restricted` while preserving local live-edit. The node sees the base via the `syco setup` bind-mount (`syco tenant up` sets `hostPathBase`); GitOps operators set their own node path.
- **`S3`** — the workspace additionally gets a cluster-scoped **read-write** writer PV/PVC `kernel-writer-<workspace>-<namespace>` at the *same* host dir. `mainframe-controller` runs a one-shot `aws s3 sync` **Job** (bucket credentials live in the Job, never in the serving pod) that mounts the writer PVC at `/kernels` and syncs the bucket prefix there; it surfaces read-only at `/etc/kernels/<namespace>/<workspace>` through the workspace's serving PV. Snapshot only for now — periodic re-sync (live S3) is deferred.

```
<base>/<ns>/<ws>  (HostPath: live · S3: synced) → mainframe-ctrl /etc/kernels/<ns>/<ws> → GetAgent RPC → transponder → agent
```

Because delivery renders per-workspace from values with no `lookup`, the chart renders identically under `helm install` and a GitOps `helm template | kubectl apply` pipeline — neither strips a kernel.

For `HostPath`, the mount *is* the host filesystem, not a copy: edits from outside the cluster (in the operator's editor) appear inside `mainframe-ctrl` on the next `read(2)`, and the transponder picks them up on its next `GetAgent` refresh.

`mainframe-controller` reads the local kernel tree and serves the content over gRPC (`GetAgent`/`ListAgents` plus the skill tools). For `S3` kernels it also creates the one-shot sync `Job` (its only write verb); `HostPath` needs none.

### Authoring a kernel

Kernel is content, not chart config — author it per workspace with the CLI (or `kubectl apply` a `Kernel` CR):

```
syco tenant kernel set research --kind hostpath --ns <tenant>

syco tenant kernel set data --kind s3 \
  --endpoint http://versitygw:7070 --bucket sycophant-tenants \
  --prefix tenant-abc/mainframe/ --credentials tenant-s3-credentials --ns <tenant>
```

A `HostPath` kernel defaults to `{ kind: HostPath }` (no `hostPath.path`): content lives at the convention location `<kernels-root>/<namespace>/<workspace>` (the CLI's bind-mounted kernels dir locally). Drop the persona files there and they appear live at `/etc/kernels/<namespace>/<workspace>`. Pass `--path <absolute-dir>` to override the source with a custom host directory instead; `syco tenant up` wires it in (CLI-only). On local k3d the custom dir must live under the bind-mounted `~/.config/sycophant/kernels` tree to be visible in the node; on a real cluster it must exist on the node the pod schedules to.

### ValidatingAdmissionPolicy on hostPath

The `cluster-gvisor-pod-policy` VAP forbids `hostPath` volumes on **all** sycophant pods — there is no per-pod exception. Local kernels are delivered through a PVC bound to a cluster-scoped PV (the `hostPath` lives on the PV, never on a pod), so no pod needs one.

### Subsystem-level config

The top-level `mainframe:` block holds operator-level settings:

```yaml
mainframe:
  image: ghcr.io/calebfaruki/mainframe-controller
  tag: latest
  pullPolicy: Always
  refreshIntervalSeconds: 60
```

`refreshIntervalSeconds` is the `Kernel` watcher's periodic reconcile cadence.

## Reference fixtures

[`examples/mainframe/`](../examples/mainframe/) holds a fixture you can copy onto the host path as a starting point:

- [`simple/`](../examples/mainframe/simple/) — minimal assistant with local tools only. Single `AGENTS.md`.

## Routing delegates to specific models

A persona file (or `AGENTS.md` itself) MAY declare a `model:` field in YAML frontmatter at the top of the file. Ownership splits across two pods:

1. The **transponder** parses the frontmatter (delimited by `---` lines, max 4 KiB), selects the `model:` name (or resolves `inherit` from the conversation log), and strips the frontmatter from the system prompt before dispatch — the LLM never sees the YAML.
2. It sends the resolved model name + stripped system + assembled history to **hangar**, which looks the name up in the operator's model registry (any name registered via `syco model set`, including aliases) and dispatches the call to that model's LLM Job.

Example. With two registered models (`fast` and `smart`):

```bash
syco model set deepseek/deepseek-v4-flash --provider openrouter --secret my-key --alias fast
syco model set deepseek/deepseek-r1 --provider openrouter --secret my-key --alias smart
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

Files without frontmatter dispatch to whichever model the request specified. If the request didn't specify one either, the runtime falls back to the **alphabetically-first registered model**. With one model registered, that's trivially the only choice. With multiple models, operators steer the fallback by naming (a model named `aaa` sorts before `mmm`) or by adding `---\nmodel: <name>\n---\n` frontmatter to `AGENTS.md` to make the choice explicit. There is no reserved `default` name; if zero models are registered, the call fails fast with a clear error.

**Audit story.** The `system_prompt_sha256` field on each assistant log entry is computed on the **pre-strip** value — i.e., the verbatim file contents the orchestrator passed. External auditors run `sha256sum agents/alice.md` on the canonical file and the value matches the log directly. No frontmatter-stripping step needed in the audit tooling.

**Failure mode.** If `model:` references a name not in the registry, the call fails fast with a `failed_precondition` error naming the missing model. Operators discover available names via `syco model list`.

## Future work

- **Non-HostPath/S3 kernel kinds** — OCI, lakeFS, git adapters ship as separate-repo crates with their own controllers. The Kernel CRD's `spec.kind` discriminator already accommodates them.
- **CLI helpers** — `syco init` to scaffold a new mainframe folder.
- **Web UI / SaaS authoring surface** — operator-facing app for editing principal content (Rails admin).

## Verification

After install, inspect the kernel from the `mainframe-ctrl` pod:

```bash
kubectl exec -n <ns> deploy/mainframe-ctrl -c ctrl -- ls -la /etc/kernels/<namespace>/<workspace>
kubectl exec -n <ns> deploy/mainframe-ctrl -c ctrl -- cat /etc/kernels/<namespace>/<workspace>/AGENTS.md
```

The workspace subdirectory should be present and the file readable. To confirm the transponder can fetch it, check the transponder logs for a successful `get_agent` refresh after startup.

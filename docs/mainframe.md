# Mainframe

The Mainframe is the workspace pod's read-only knowledge mount. It holds the principal-authored files that drive agent behavior — most importantly the `AGENTS.md` that the workspace runtime passes to Tightbeam as the agent's system prompt.

See decisions [`006-mainframe-as-readonly-mount`](../../vault/projects/sycophant/decisions/006-mainframe-as-readonly-mount.md), [`007-entrypoint-driven-runtime`](../../vault/projects/sycophant/decisions/007-entrypoint-driven-runtime.md), and [`010-out-of-cluster-admin-and-mainframe-source-kinds`](../../vault/projects/sycophant/decisions/010-out-of-cluster-admin-and-mainframe-source-kinds.md) for the architectural background. ADR 010 supersedes the S3-canonical model from ADR 008 with a pluggable source-kind discriminator. v0 ships only `kind: HostPath`.

## Layout conventions

The mainframe is the principal's OS. Real OSes have non-configurable layouts (`/etc`, `/var`, `/usr`); programs that respect them just work. Sycophant's mainframe follows the same principle: structure is conventional, the source path is configurable. If every principal would pick the same answer, the chart doesn't ask.

Mount points (fixed):

- `/etc/mainframe/` — read-only knowledge tree. Mounted into both `transponder` and `mainframe-runtime` containers via a `hostPath` volume from the host directory the workspace declared.
- `/workspace/` — the agent's writable working directory (per-workspace PVC).
- `/tmp/`, `/home/agent/` — ephemeral scratch.

Layout inside `/etc/mainframe/`:

- `AGENTS.md` — the agent's system prompt source. The workspace runtime reads it on every turn and passes the contents as the system prompt for every Tightbeam call. Aligns with the [Linux Foundation Agentic AI Foundation's AGENTS.md convention](https://agents.md/).
- `agents/<name>/AGENTS.md` — per-delegate persona for orchestrator-style agents that route via `llm_call`. The convention is recursive: each delegate is a sub-agent rooted at its own AGENTS.md.
- `skills/<name>.md` — free-form markdown describing how to perform a focused task. The root AGENTS.md tells the LLM "skills live at `/etc/mainframe/skills/`; list and read as needed." Lets the principal build a library of how-to-do-X documents that don't bloat the system prompt.
- `<topic>/` — free-form subdirectories for anything else (project context, glossaries, FAQs). The root AGENTS.md points at what's relevant.

Sycophant's interpretation of AGENTS.md is "the agent's file at this level of the OS." The canonical AGENTS.md spec is silent on persona content (it scopes itself to project context); using it recursively for delegate personas extends the convention rather than contradicting it.

Trust contract:

- The cluster never writes to the Mainframe. All writes happen at the source, controlled by the principal — directly on the host filesystem.
- Each workspace has its **own** mainframe — different AGENTS.md, different skills, different sub-agents. Multiple workspaces in the same namespace are *different agents*, not copies of one.

## How it's wired

Per ADR 010, every workspace declares an `instructions:` field — an absolute path to a directory on the host node. The chart renders a `Kernel` CR with `spec.kind: HostPath`, and mainframe-controller materializes a workspace Pod that mounts that host directory at `/etc/mainframe` via a `hostPath` volume (`type: Directory`, `readOnly: true`).

```
host filesystem (instructions:) → kubelet hostPath mount → workspace pod /etc/mainframe → mainframe-runtime → agent
```

The workspace pod sees changes immediately: the mount is the host filesystem, not a copy. Edits from outside the cluster (in the operator's editor) appear inside the pod on the next `read(2)`. The transponder re-reads `AGENTS.md` on every turn (Phase 2 of ADR 010 implementation).

`mainframe-controller` watches Kernel CRs and reconciles them. For `kind: HostPath` the reconciliation is a no-op — kubelet handles the mount. The controller stays deployed as scaffolding for future non-HostPath kernel kinds (which ship as separate-repo adapters per ADR 010); v0's chart still includes it for symmetry and to keep the Kernel CR addressable.

### `instructions:` (per workspace)

Single shape: an absolute host filesystem path.

```yaml
workspaces:
  research:
    image: ghcr.io/myorg/mainframe-runtime
    tag: "1.0"
    instructions: /Users/me/sycophant/workspaces/research

  coding:
    image: ghcr.io/myorg/mainframe-runtime
    tag: "1.0"
    instructions: /Users/me/sycophant/workspaces/coding
    chambers:
      - git-ops
```

The schema (`charts/sycophant-tenant/values.schema.json`) requires the value to match `^/.+`. The directory must exist on the host node where the workspace pod runs; kubelet's `hostPath` mount with `type: Directory` fails the pod's mount step if it doesn't.

### ValidatingAdmissionPolicy on hostPath

The Sandbox VAP forbids hostPath volumes by default. v0 relaxes the rule for exactly one volume named `mainframe`, mounted at `/etc/mainframe`, with `readOnly: true`. Any other hostPath usage on a Sandbox is rejected.

### Subsystem-level config

The top-level `mainframe:` block holds operator-level settings:

```yaml
mainframe:
  image: ghcr.io/calebfaruki/mainframe-controller
  tag: latest
  pullPolicy: Always
  refreshIntervalSeconds: 60
```

`refreshIntervalSeconds` is the periodic reconcile cadence; v0 reconciliation is a no-op for HostPath, so the value mostly affects log volume.

## Reference fixtures

[`examples/mainframe/`](../examples/mainframe/) holds two fixtures you can copy onto the host path as a starting point:

- [`simple/`](../examples/mainframe/simple/) — minimal assistant with local tools only. Single `AGENTS.md`.
- [`orchestrator/`](../examples/mainframe/orchestrator/) — orchestrator that routes between named delegates (Alice, Bob) via `llm_call`. Root `AGENTS.md` plus per-delegate `agents/<name>/AGENTS.md`.

## Routing delegates to specific models

A persona file (or `AGENTS.md` itself) MAY declare a `model:` field in YAML frontmatter at the top of the file. When the orchestrator passes such a file's contents as the `system_prompt` argument to `llm_call`, the Tightbeam controller:

1. Parses the frontmatter (delimited by `---` lines, max 4 KiB).
2. Looks up `model:` in the operator's model registry (any name registered via `syco model set`, including aliases).
3. Dispatches the call to that model.
4. Strips the frontmatter from the system prompt before forwarding the body to the LLM Job — the LLM never sees the YAML.

Example. With two registered models (`fast` and `smart`):

```bash
syco model set anthropic.haiku --provider anthropic --secret my-key --alias fast
syco model set anthropic.sonnet --provider anthropic --secret my-key --alias smart
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

**Audit story.** The `system_prompt_sha256` field on each assistant log entry is computed on the **pre-strip** value — i.e., the verbatim file contents the orchestrator passed. External auditors run `sha256sum agents/alice/AGENTS.md` on the canonical file and the value matches the log directly. No frontmatter-stripping step needed in the audit tooling.

**Failure mode.** If `model:` references a name not in the registry, the call fails fast with a `failed_precondition` error naming the missing model. Operators discover available names via `syco model list`.

## Future work

- **Non-HostPath kernel kinds** — S3, OCI, lakeFS, git adapters per ADR 010 ship as separate-repo crates with their own controllers. The Kernel CRD's `spec.kind` discriminator already accommodates them.
- **CLI helpers** — `syco init` to scaffold a new mainframe folder.
- **Web UI / SaaS authoring surface** — operator-facing app for editing principal content (per ADR 010's Rails admin discussion).

## Verification

After install:

```bash
kubectl exec -n <ns> <workspace-pod> -c mainframe-runtime -- ls -la /etc/mainframe
kubectl exec -n <ns> <workspace-pod> -c mainframe-runtime -- cat /etc/mainframe/AGENTS.md
```

The mount should be present and the file readable. Writes from inside the pod must fail (read-only mount).

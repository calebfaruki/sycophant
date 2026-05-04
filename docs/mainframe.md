# Mainframe

The Mainframe is the workspace pod's read-only knowledge mount. It holds the principal-authored files that drive agent behavior — most importantly the `ENTRYPOINT.md` that the workspace runtime passes to Tightbeam as the agent's system prompt.

See decisions [`006-mainframe-as-readonly-mount`](../../vault/projects/sycophant/decisions/006-mainframe-as-readonly-mount.md) and [`007-entrypoint-driven-runtime`](../../vault/projects/sycophant/decisions/007-entrypoint-driven-runtime.md) for the architectural background.

## Conventions

- The Mainframe is mounted into each workspace pod's `workspace-tools` container at `/etc/mainframe`, read-only.
- `ENTRYPOINT.md` lives at `/etc/mainframe/ENTRYPOINT.md`. The workspace runtime reads it once at startup (entrypoint runtime mode) and passes the contents as the system prompt for every Tightbeam call in that workspace.
- Other files (agent definitions, skills, project context, memos) live alongside `ENTRYPOINT.md` and are reachable by the LLM via the standard filesystem tools (`read_file`, `list_directory`, etc.). The entrypoint's prose tells the LLM how to use them.
- The cluster never writes to the Mainframe. All writes happen at the source, controlled by the principal.

## Adapters

The Mainframe is populated by an **adapter**, chosen at Helm-install time via the `mainframe.adapter` value. v1 supports one adapter:

### `local` (v1)

A `hostPath` directory on the cluster node, exposed to workspace pods through a cluster-scoped `PersistentVolume` and a `PersistentVolumeClaim`.

```yaml
mainframe:
  adapter: local
  local:
    hostPath: /var/lib/sycophant/mainframe   # required; path on the node
    storageClass: ""                          # optional; empty = cluster default
    capacity: "1Gi"                           # optional; PV capacity request
```

The PV is created by the chart and references the host path; the PVC binds to it; the Sandbox podTemplate references the PVC by name. The Sandbox VAP (`templates/sandbox-vap.yaml`) prohibits hostPath volumes inside Sandbox specs, but PVs are cluster-scoped and outside the VAP's view — the PV+PVC chain satisfies both the security policy (no hostPath in Sandbox specs) and the use case (host-folder access from workspace pods).

**Why this honors the security thesis:** `PersistentVolume` creation requires cluster-admin RBAC. Path selection is therefore an operator-time decision, not a Sandbox-creation-time decision. An in-pod adversary cannot redirect the Mainframe to a different host path because pod mounts are immutable post-creation and the adversary lacks the RBAC to create new PVs/PVCs. Direct hostPath in the Sandbox spec would have allowed Sandbox-creators (a wider set than cluster-admins) to choose paths; the PV+PVC pattern is structurally tighter.

**Operator setup steps:**

1. Choose a host path and ensure the directory exists on every node where workspace pods may schedule. (For single-node Kind clusters, this is one machine.)
2. Populate the directory with at minimum an `ENTRYPOINT.md`. Typical layout:

   ```
   /var/lib/sycophant/mainframe/
   ├── ENTRYPOINT.md
   ├── agents/
   │   ├── alice/...
   │   └── bob/...
   └── skills/
       └── ...
   ```

3. Set `mainframe.local.hostPath` in your Helm values.
4. `helm install` (or `helm upgrade`) sycophant.

The principal authors and edits files in this directory directly — no cluster API involvement. The workspace pod sees changes on the next file read.

## Reference ENTRYPOINT.md fixtures

[`examples/mainframe/`](../examples/mainframe/) holds two fixtures you can copy onto the host path as a starting point:

- [`simple/`](../examples/mainframe/simple/) — minimal assistant with local tools only. Single `ENTRYPOINT.md`.
- [`orchestrator/`](../examples/mainframe/orchestrator/) — orchestrator that routes between named delegates (Alice, Bob) via `llm_call`. `ENTRYPOINT.md` plus per-delegate persona files under `agents/<name>/`.

## Routing delegates to specific models

A persona file (or `ENTRYPOINT.md` itself) MAY declare a `model:` field in YAML frontmatter at the top of the file. When the orchestrator passes such a file's contents as the `system_prompt` argument to `llm_call`, the Tightbeam controller:

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

Files without frontmatter dispatch to whichever model the request specified. If the request didn't specify one either, the runtime falls back to the **alphabetically-first registered model**. With one model registered, that's trivially the only choice. With multiple models, operators steer the fallback by naming (a model named `aaa` sorts before `mmm`) or by adding `---\nmodel: <name>\n---\n` frontmatter to `ENTRYPOINT.md` to make the choice explicit. There is no reserved `default` name; if zero models are registered, the call fails fast with a clear error.

**Audit story.** The `system_prompt_sha256` field on each assistant log entry is computed on the **pre-strip** value — i.e., the verbatim file contents the orchestrator passed. External auditors run `sha256sum agents/alice/system_prompt.md` on the canonical file and the value matches the log directly. No frontmatter-stripping step needed in the audit tooling.

**Failure mode.** If `model:` references a name not in the registry, the call fails fast with a `failed_precondition` error naming the missing model. Operators discover available names via `syco model list`.

## Future adapters

Out of scope for v1; not yet designed. Expected shapes:

- `git`: a `git-sync` sidecar populates an `emptyDir`; workspace mounts the same volume read-only through git-sync's atomic symlink.
- Others (S3, etc.) as use cases appear.

## Verification

After install:

```bash
kubectl exec -n <ns> <workspace-pod> -c workspace-tools -- ls -la /etc/mainframe
kubectl exec -n <ns> <workspace-pod> -c workspace-tools -- cat /etc/mainframe/ENTRYPOINT.md
```

The mount should be present and the file readable. Writes from inside the pod must fail (read-only mount).

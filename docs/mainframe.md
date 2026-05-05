# Mainframe

The Mainframe is the workspace pod's read-only knowledge mount. It holds the principal-authored files that drive agent behavior — most importantly the `ENTRYPOINT.md` that the workspace runtime passes to Tightbeam as the agent's system prompt.

See decisions [`006-mainframe-as-readonly-mount`](../../vault/projects/sycophant/decisions/006-mainframe-as-readonly-mount.md), [`007-entrypoint-driven-runtime`](../../vault/projects/sycophant/decisions/007-entrypoint-driven-runtime.md), and [`008-mainframe-as-principal-os-sourced-via-s3`](../../vault/projects/sycophant/decisions/008-mainframe-as-principal-os-sourced-via-s3.md) for the architectural background. ADR 008 supersedes the v1 hostPath/PV mechanism from 006/007 with an S3-canonical, per-workspace model.

## Conventions

- The Mainframe is mounted into each workspace pod's `transponder` and `mainframe-runtime` containers at `/etc/mainframe`, read-only.
- `ENTRYPOINT.md` lives at `/etc/mainframe/ENTRYPOINT.md`. The workspace runtime reads it once at startup (entrypoint runtime mode) and passes the contents as the system prompt for every Tightbeam call in that workspace.
- Other files (agent definitions, skills, project context, memos) live alongside `ENTRYPOINT.md` and are reachable by the LLM via the standard filesystem tools (`read_file`, `list_directory`, etc.). The entrypoint's prose tells the LLM how to use them.
- The cluster never writes to the Mainframe. All writes happen at the source, controlled by the principal.
- Each workspace has its **own** mainframe — different ENTRYPOINT.md, different skills, different sub-agents. Multiple workspaces in the same namespace are *different agents*, not copies of one.

## How it's wired

Per ADR 008 stage 4, every workspace declares an `instructions:` field; mainframe-controller pulls each workspace's source into a per-CR subdirectory on its PVC; workspace pods mount that PVC at `/etc/mainframe` with `subPath: <workspace-name>`. The data flow is one-directional and identical regardless of source mode:

```
source (S3) → mainframe-controller (pulls into PVC subdir) → workspace pod (read-only mount) → mainframe-runtime → agent
```

### Sync behavior

mainframe-controller polls each Mainframe CR's source every `refreshIntervalSeconds` (default 60s; the e2e values use 30s). Each tick is a single LIST + (selective) GET pass against the bucket.

**What propagates:**
- **Adds** — new files in the source appear at `/etc/mainframe/...` on the next tick.
- **Edits** — modified content overwrites the local copy via atomic write (write-to-temp + rename), so workspace-pod readers see either the old file or the new file, never a partial mix.
- **Deletes** — files removed from the source are removed from the local PVC. The principal can revoke content by deleting it; the agent's view converges to whatever currently exists in the source. Deletes are applied with `--delete-after` semantics: only after a fully-successful list-and-fetch pass. A failed listing or any GET error skips the delete phase that tick and retries on the next round.

**Bandwidth profile:**
- mainframe-controller persists a per-workspace ETag map at `<data_dir>/.etags/<workspace>.json`. On each tick, GETs are skipped for objects whose listing ETag matches the stored one and whose local file still exists.
- Most S3-compatible backends (R2, AWS, MinIO, Garage) return content-derived ETags. Skip-unchanged works there: bandwidth scales with the number of changed files per tick, not the bucket size.
- Versitygw's posix backend returns empty ETags for files placed via hostPath (which is how bundled mode works). The controller treats empty ETags as "no useful info" and falls through to a GET. Bundled mode therefore re-fetches every file every tick — fine for local self-host where the network cost is zero.
- Correctness is identical in both cases. The optimization only affects bandwidth.

### `instructions:` (per workspace)

The user-facing key on each workspace. Two forms:

```yaml
workspaces:
  research:
    image: ...
    # String: absolute local path. The chart provisions a per-workspace
    # Versitygw deployment backed by this folder; mainframe-controller pulls
    # from that Versitygw like any S3 source.
    instructions: /Users/me/personal/research

  coding:
    image: ...
    # Object: external S3 endpoint, user-managed (R2 / AWS / Garage / etc.).
    instructions:
      endpoint: https://r2.example.com
      bucket: coding-mainframe
      secretName: coding-s3-creds
      region: auto       # optional
```

Schema (`charts/sycophant/values.schema.json`) enforces exactly one of: string (matching `^/.+`) or object with required `endpoint`, `bucket`, `secretName`. Mixing across workspaces in one namespace is allowed.

### Source shape vs deployment shape (two axes, orthogonal)

The two `instructions:` forms describe **where the principal authors content** — that's the *source shape*. It's independent of how sycophant is deployed (the *deployment shape*). All four combinations are valid:

| | Bundled source (string) | External source (object) |
|---|---|---|
| **Local self-host** | k3d / kind / OrbStack with chart-managed Versitygw on a host folder. Live editing in your editor. | Local cluster pulls from a remote S3 endpoint (e.g., R2). Useful for sharing prompts across teams or machines, no chart-managed gateway. |
| **Multi-tenant (SaaS, in-house)** | Not supported — bundled mode requires a host filesystem the cluster can see, which multi-tenant deployments don't have. | Standard pattern: bucket(s) per tenant, IAM at the bucket layer, no chart-side gateways. |

The "bundled = local, external = SaaS" framing is wrong. Specifically: a solo developer who keeps their mainframe content in R2 so they can use it from a laptop, a desktop, and a CI environment is local-deployment + external-source. The chart supports this with no special handling — pick the source shape that matches where your content lives, regardless of where sycophant runs.

### Bundled mode (string `instructions:`)

When `instructions:` is a string, the chart renders three resources for that workspace:

1. **Secret** (`<release>-<workspace>-mainframe-s3-creds`) with random `AWS_ACCESS_KEY_ID` / `AWS_SECRET_ACCESS_KEY`.
2. **Deployment** (`<release>-<workspace>-mainframe-s3`) running [Versitygw](https://github.com/versity/versitygw) with the user's path mounted at `/data/instructions` (a `hostPath` volume); Versitygw's posix backend exposes that directory as the bucket `instructions`.
3. **Service** exposing port `7070`.

The Mainframe CR for the workspace points at the in-cluster Versitygw Service URL, bucket `instructions`, with the generated Secret as credentials. Files are stored on disk as real files — no opaque object format — so the principal can author and edit content directly from the host.

### External mode (object `instructions:`)

When `instructions:` is an object, no Versitygw is rendered. The Mainframe CR points directly at the user-supplied endpoint with the user-supplied secret. This mode supports any S3-compatible endpoint: cloud S3 (AWS, R2, Backblaze), self-managed gateways (Garage, MinIO), or another bundled Versitygw maintained outside this chart.

### Subsystem-level config

The top-level `mainframe:` block holds operator-level settings:

```yaml
mainframe:
  controller:
    image: ghcr.io/calebfaruki/mainframe-controller
    tag: latest
    pullPolicy: Always
    dataCapacity: "10Gi"   # PVC capacity for the controller's data volume
  versitygw:
    image: versity/versitygw
    tag: "v1.0.18"
    pullPolicy: IfNotPresent
  refreshIntervalSeconds: 60
```

`mainframe.controller.*` configures the controller Deployment. `mainframe.versitygw.*` selects the image used for bundled-mode deployments. `refreshIntervalSeconds` is the periodic re-pull cadence applied to every workspace's mainframe.

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

## Future work

- **CLI helpers** — `syco init` to scaffold a new mainframe folder, `syco mainframe push` to upload to a remote endpoint. (ADR 008 stage 5.)
- **Web UI / SaaS authoring surface** — out-of-namespace web app for editing principal content; same S3 destination. (ADR 008 stage 5.)
- **Versitygw alternatives** — Garage, RustFS, or others may be revisited if Versitygw friction surfaces. The chart's bundled-mode interface is narrow (an in-cluster S3 endpoint), so swaps are mechanical.

## Verification

After install:

```bash
kubectl exec -n <ns> <workspace-pod> -c mainframe-runtime -- ls -la /etc/mainframe
kubectl exec -n <ns> <workspace-pod> -c mainframe-runtime -- cat /etc/mainframe/ENTRYPOINT.md
```

The mount should be present and the file readable. Writes from inside the pod must fail (read-only mount).

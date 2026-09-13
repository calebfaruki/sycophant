# Stdlib toolset

A convention, not a built-in: a toolset entry pointing at the base toolset
image (`toolset`, image dir `images/toolset/`), bound explicitly like any
other. It bundles five built-in tools served directly by `toolset-runtime`
(no `/etc/toolset/dispatch` shell layer required):

| Tool     | Description                                                          |
|----------|----------------------------------------------------------------------|
| `Shell`  | Run a shell command, return stdout/stderr/exit-code                  |
| `Read`   | Read a text file as line-numbered `LINE\|CONTENT` (1 MiB cap)        |
| `Write`  | Write content to a file (parents auto-created, full overwrite)       |
| `Edit`   | Replace an exact unique substring in a file                          |
| `Search` | List files by basename or grep content (ripgrep-backed)              |

Tool names are PascalCase (the canonical LLM-facing identifier). The
K8s Job name the toolset builds for each call is kebab-cased
(`tool-read-<call_id_prefix>`) to satisfy RFC 1123.

`keepalive: true` keeps one toolset pod alive per workspace for the
workspace's lifetime — there is no per-call cold start.

## Extending the base image

The capability manifest is built from the image's baked `tools.yaml`, not a
label. A derived image's `tools.yaml` REPLACES the base's — the reader reads one
file — so to extend the stdlib toolset you author a single `tools.yaml` listing
ALL five stdlib entries alongside your additions, bake it, and point the source
label at it:

```dockerfile
FROM ghcr.io/calebfaruki/toolset:latest

# Lists all five stdlib tools plus your own; see images/toolset/tools.yaml for
# the schema format. This file is the single source of truth for the image.
COPY tools.yaml /etc/toolset/tools.yaml
LABEL md.sycophant.tools.source="/etc/toolset/tools.yaml"

# tool dispatcher for non-built-in tools
COPY dispatch /etc/toolset/dispatch
RUN chmod +x /etc/toolset/dispatch
```

The toolset-runtime entrypoint inherited from the base image routes
built-in tool names to its in-process implementation and falls through
to `/etc/toolset/dispatch <tool>` for anything else.

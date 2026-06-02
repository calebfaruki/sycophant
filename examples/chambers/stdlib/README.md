# Stdlib chamber

The default chamber bound to every workspace. It bundles four built-in
tools served directly by `airlock-runtime` (no `/etc/chamber/dispatch`
shell layer required):

| Tool            | Description                                                |
|-----------------|------------------------------------------------------------|
| `Bash`          | Run a shell command, return stdout/stderr/exit-code        |
| `ReadFile`      | Read file contents                                         |
| `WriteFile`     | Write content to a file (creating it if missing)           |
| `ListDirectory` | List directory contents (sorted)                           |

Tool names are PascalCase (the canonical LLM-facing identifier). The
K8s Job name airlock builds for each call is kebab-cased
(`airlock-read-file-<call_id_prefix>`) to satisfy RFC 1123.

`keepalive: true` keeps one chamber pod alive per workspace for the
workspace's lifetime — there is no per-call cold start.

## Extending the base image

OCI image labels REPLACE on `FROM` — the controller does NOT merge a
derived image's `md.sycophant.tools` label with its base. To extend
the stdlib chamber with additional tools, you must re-declare ALL of
the stdlib entries alongside your additions:

```dockerfile
FROM ghcr.io/calebfaruki/airlock-chamber:latest

LABEL md.sycophant.tools='[\
  {"name": "Bash",          "description": "...", "args": {"command": {"type": "string", "required": true, "env": "command", "description": "..."}}},\
  {"name": "ReadFile",      "description": "...", "args": {"path":    {"type": "string", "required": true, "env": "path",    "description": "..."}}},\
  {"name": "WriteFile",     "description": "...", "args": {"path":    {"type": "string", "required": true, "env": "path",    "description": "..."}, "content": {"type": "string", "required": true, "env": "content", "description": "..."}}},\
  {"name": "ListDirectory", "description": "...", "args": {"path":    {"type": "string", "required": true, "env": "path",    "description": "..."}}},\
  {"name": "MyTool",        "description": "...", "args": {}}\
]'

# tool dispatcher for non-built-in tools
COPY dispatch /etc/chamber/dispatch
RUN chmod +x /etc/chamber/dispatch
```

The airlock-runtime entrypoint inherited from the base image routes
built-in tool names to its in-process implementation and falls through
to `/etc/chamber/dispatch <tool>` for anything else.

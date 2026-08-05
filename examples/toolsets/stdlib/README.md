# Stdlib toolset

The default toolset bound to every workspace. It bundles five built-in
tools served directly by `toolset-runtime` (no `/etc/toolset/dispatch`
shell layer required):

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

OCI image labels REPLACE on `FROM` — the controller does NOT merge a
derived image's `md.sycophant.tools` label with its base. To extend
the stdlib toolset with additional tools, you must re-declare ALL of
the stdlib entries alongside your additions:

```dockerfile
FROM ghcr.io/calebfaruki/toolset:latest

LABEL md.sycophant.tools='[\
  {"name": "Shell",  "description": "...", "args": {"command": {"type": "string", "required": true, "env": "command", "description": "..."}}},\
  {"name": "Read",   "description": "...", "args": {"path":    {"type": "string", "required": true, "env": "path",    "description": "..."}}},\
  {"name": "Write",  "description": "...", "args": {"path":    {"type": "string", "required": true, "env": "path",    "description": "..."}, "content": {"type": "string", "required": true, "env": "content", "description": "..."}}},\
  {"name": "Edit",   "description": "...", "args": {"path":    {"type": "string", "required": true, "env": "path",    "description": "..."}, "old_string": {"type": "string", "required": true, "env": "old_string", "description": "..."}, "new_string": {"type": "string", "required": true, "env": "new_string", "description": "..."}}},\
  {"name": "Search", "description": "...", "args": {"target":  {"type": "string", "required": true, "env": "target",  "description": "..."}, "pattern":    {"type": "string", "required": true, "env": "pattern",    "description": "..."}}},\
  {"name": "MyTool", "description": "...", "args": {}}\
]'

# tool dispatcher for non-built-in tools
COPY dispatch /etc/toolset/dispatch
RUN chmod +x /etc/toolset/dispatch
```

The toolset-runtime entrypoint inherited from the base image routes
built-in tool names to its in-process implementation and falls through
to `/etc/toolset/dispatch <tool>` for anything else.

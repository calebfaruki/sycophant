FROM alpine:3.21
RUN apk add --no-cache ripgrep git \
 && git config --system --add safe.directory '*' \
 && git config --system user.name 'sycophant' \
 && git config --system user.email 'noreply@invalid'
LABEL md.sycophant.tools='[\
  {"name": "Shell",  "description": "Run a shell command and return stdout, stderr, exit code.", "args": {\
    "command": {"type": "string",  "required": true,  "env": "command", "description": "Shell command to execute under sh -c."},\
    "timeout": {"type": "integer", "required": false, "env": "timeout", "description": "Foreground timeout in seconds (default 60, hard cap 600)."},\
    "workdir": {"type": "string",  "required": false, "env": "workdir", "description": "Working directory override (default workspace cwd)."}\
  }},\
  {"name": "Read",   "description": "Read a text file, line-numbered LINE|CONTENT per row. 1 MiB hard cap.", "args": {\
    "path":   {"type": "string",  "required": true,  "env": "path",   "description": "File path."},\
    "offset": {"type": "integer", "required": false, "env": "offset", "description": "1-indexed starting line (default 1)."},\
    "limit":  {"type": "integer", "required": false, "env": "limit",  "description": "Max lines to return (default unlimited)."}\
  }},\
  {"name": "Write",  "description": "Write content to a file, full overwrite. Parent directories auto-created.", "args": {\
    "path":    {"type": "string", "required": true, "env": "path",    "description": "File path."},\
    "content": {"type": "string", "required": true, "env": "content", "description": "Content to write."}\
  }},\
  {"name": "Edit",   "description": "Replace exact unique substring in a file. Errors on 0 or >1 matches.", "args": {\
    "path":       {"type": "string", "required": true, "env": "path",       "description": "File path."},\
    "old_string": {"type": "string", "required": true, "env": "old_string", "description": "Substring to find (must appear exactly once)."},\
    "new_string": {"type": "string", "required": true, "env": "new_string", "description": "Replacement substring."}\
  }},\
  {"name": "Search", "description": "Search files (target=files lists; target=content greps). Ripgrep-backed.", "args": {\
    "target":  {"type": "string",  "required": true,  "env": "target",  "description": "\"content\" greps inside files; \"files\" lists files by basename substring."},\
    "pattern": {"type": "string",  "required": true,  "env": "pattern", "description": "Regex (content) or case-insensitive basename substring (files; empty lists all)."},\
    "path":    {"type": "string",  "required": false, "env": "path",    "description": "Root path (default workspace cwd)."},\
    "glob":    {"type": "string",  "required": false, "env": "glob",    "description": "Optional path glob filter."},\
    "limit":   {"type": "integer", "required": false, "env": "limit",   "description": "Max results (default 50; offset+limit <= 1000)."},\
    "offset":  {"type": "integer", "required": false, "env": "offset",  "description": "Skip first N results (default 0)."}\
  }}\
]'
ARG TARGETARCH
COPY airlock-runtime-linux-${TARGETARCH} /usr/local/bin/airlock-runtime
RUN chmod +x /usr/local/bin/airlock-runtime
ENTRYPOINT ["airlock-runtime"]

FROM alpine:3.21
LABEL md.sycophant.tools='[\
  {"name": "Bash",          "description": "Run a shell command and return stdout, stderr, exit code.", "args": {\
    "command": {"type": "string", "required": true, "env": "command", "description": "Shell command to execute."}\
  }},\
  {"name": "ReadFile",      "description": "Read file contents.", "args": {\
    "path":    {"type": "string", "required": true, "env": "path",    "description": "File path."}\
  }},\
  {"name": "WriteFile",     "description": "Write content to a file, creating it if missing.", "args": {\
    "path":    {"type": "string", "required": true, "env": "path",    "description": "File path."},\
    "content": {"type": "string", "required": true, "env": "content", "description": "Content to write."}\
  }},\
  {"name": "ListDirectory", "description": "List directory contents (sorted).", "args": {\
    "path":    {"type": "string", "required": true, "env": "path",    "description": "Directory path."}\
  }}\
]'
ARG TARGETARCH
COPY airlock-runtime-linux-${TARGETARCH} /usr/local/bin/airlock-runtime
RUN chmod +x /usr/local/bin/airlock-runtime
ENTRYPOINT ["airlock-runtime"]

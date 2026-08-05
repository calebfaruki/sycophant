FROM alpine:3.21
LABEL md.sycophant.tools='[\
  {"name": "git-status",   "description": "Show the working tree status in the workspace.", "args": {}},\
  {"name": "git-log",      "description": "Show recent commit history (10 most recent) for the workspace.", "args": {}},\
  {"name": "git-diff",     "description": "Show unstaged changes in the workspace.", "args": {}},\
  {"name": "git-branch",   "description": "List local branches in the workspace.", "args": {}},\
  {"name": "git-fetch",    "description": "Fetch refs from the default remote.", "args": {}},\
  {"name": "git-pull",     "description": "Pull the current branch from its tracked remote.", "args": {}},\
  {"name": "git-push",     "description": "Push the current branch to its tracked remote.", "args": {}},\
  {"name": "git-clone",    "description": "Clone a git repository.", "args": {\
    "url":  {"type": "string", "required": true, "env": "URL",  "description": "Repository URL."},\
    "dest": {"type": "string", "required": true, "env": "DEST", "description": "Destination directory (relative to /workspace)."}\
  }},\
  {"name": "git-checkout", "description": "Check out a branch, tag, or commit.", "args": {\
    "ref": {"type": "string", "required": true, "env": "REF", "description": "Branch, tag, or commit to check out."}\
  }},\
  {"name": "git-commit",   "description": "Commit currently-staged changes.", "args": {\
    "message": {"type": "string", "required": true, "env": "MESSAGE", "description": "Commit message."}\
  }}\
]'
RUN apk add --no-cache git openssh-client
COPY dispatch /etc/toolset/dispatch
RUN chmod +x /etc/toolset/dispatch
ARG TARGETARCH
COPY toolset-runtime-linux-${TARGETARCH} /usr/local/bin/toolset-runtime
RUN chmod +x /usr/local/bin/toolset-runtime
ENTRYPOINT ["toolset-runtime"]

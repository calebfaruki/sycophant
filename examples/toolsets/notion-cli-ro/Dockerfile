FROM alpine:3.21
LABEL md.sycophant.tools='[\
  {"name": "notion-search", "description": "Full-text search across Notion pages and databases the integration is connected to.", "args": {\
    "query": {"type": "string", "required": true, "env": "QUERY", "description": "Search query."}\
  }},\
  {"name": "notion-page-get", "description": "Retrieve a Notion page rendered as Markdown.", "args": {\
    "page_id": {"type": "string", "required": true, "env": "PAGE_ID", "description": "Notion page ID."}\
  }},\
  {"name": "notion-database-query", "description": "Query rows in a Notion database.", "args": {\
    "database_id": {"type": "string", "required": true, "env": "DATABASE_ID", "description": "Notion database ID."},\
    "filter":      {"type": "string", "required": false, "env": "FILTER",      "description": "Optional filter as a JSON object string; pass {} for none."}\
  }},\
  {"name": "notion-block-children-list", "description": "List child blocks of a page or block (raw structure when Markdown loses fidelity).", "args": {\
    "block_id": {"type": "string", "required": true, "env": "BLOCK_ID", "description": "Notion page or block ID."}\
  }},\
  {"name": "notion-block-get", "description": "Read a single block as raw structure.", "args": {\
    "block_id": {"type": "string", "required": true, "env": "BLOCK_ID", "description": "Notion block ID."}\
  }},\
  {"name": "notion-comment-list", "description": "List comments on a Notion page or block.", "args": {\
    "block_id": {"type": "string", "required": true, "env": "BLOCK_ID", "description": "Notion page or block ID."}\
  }},\
  {"name": "notion-users-list", "description": "List users in the workspace (resolves @-references).", "args": {}},\
  {"name": "notion-user-get", "description": "Resolve a single user_id to name + avatar.", "args": {\
    "user_id": {"type": "string", "required": true, "env": "USER_ID", "description": "Notion user ID."}\
  }},\
  {"name": "notion-whoami", "description": "Return the integration bot identity (health check + audit reveal).", "args": {}}\
]'
RUN apk add --no-cache curl bash ca-certificates jq make \
    && curl -fsSL https://ntn.dev | bash \
    && (test -x /root/.local/bin/ntn && mv /root/.local/bin/ntn /usr/local/bin/ntn \
        || test -x /usr/local/bin/ntn)
# Pin the Notion API version so `ntn` doesn't try to fetch the OpenAPI
# spec at runtime to auto-detect it. The egress allowlist permits
# api.notion.com only; the spec host isn't reachable, so without this
# every call fails with "Failed to fetch OpenAPI spec".
ENV NOTION_API_VERSION=2022-06-28
COPY Makefile /etc/toolset/Makefile
COPY dispatch /etc/toolset/dispatch
RUN chmod +x /etc/toolset/dispatch
ARG TARGETARCH
COPY toolset-runtime-linux-${TARGETARCH} /usr/local/bin/toolset-runtime
RUN chmod +x /usr/local/bin/toolset-runtime
ENTRYPOINT ["toolset-runtime"]

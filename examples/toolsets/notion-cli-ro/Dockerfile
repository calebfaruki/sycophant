ARG BASE_IMAGE=toolset-base:local
FROM ${BASE_IMAGE}
# Bake the canonical tool schema into the image. tools.yaml is the single
# source of truth; `syco toolset manifest` reads it back out of the built image.
COPY tools.yaml /etc/toolset/tools.yaml
LABEL md.sycophant.tools.source="/etc/toolset/tools.yaml"
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

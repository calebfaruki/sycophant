ARG BASE_IMAGE=toolset-base:local
FROM ${BASE_IMAGE}
RUN apk add --no-cache ripgrep git \
 && git config --system --add safe.directory '*' \
 && git config --system user.name 'sycophant' \
 && git config --system user.email 'noreply@invalid'
# Bake the canonical tool schema into the image. tools.yaml is the single
# source of truth; `syco toolset manifest` reads it back out of the built image
# to build the capability manifest. The schema is never hand-restated here.
COPY tools.yaml /etc/toolset/tools.yaml
LABEL md.sycophant.tools.source="/etc/toolset/tools.yaml"

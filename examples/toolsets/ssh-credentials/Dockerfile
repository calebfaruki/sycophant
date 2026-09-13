ARG BASE_IMAGE=toolset-base:local
FROM ${BASE_IMAGE}
# Bake the canonical tool schema into the image. tools.yaml is the single
# source of truth; `syco toolset manifest` reads it back out of the built image.
COPY tools.yaml /etc/toolset/tools.yaml
LABEL md.sycophant.tools.source="/etc/toolset/tools.yaml"
COPY dispatch /etc/toolset/dispatch
RUN chmod +x /etc/toolset/dispatch

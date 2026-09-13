ARG BASE_IMAGE=toolset-base:local
FROM ${BASE_IMAGE}
COPY tools.yaml /etc/toolset/tools.yaml
LABEL md.sycophant.tools.source="/etc/toolset/tools.yaml"
RUN apk add --no-cache git openssh-client
COPY dispatch /etc/toolset/dispatch
RUN chmod +x /etc/toolset/dispatch

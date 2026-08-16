# The one toolset base image: its entrypoint IS the toolset runtime. Every
# toolset image builds FROM this and adds only its tools, so no dependent
# restates the runtime copy, permission, and entrypoint steps.
FROM alpine:3.21
ARG TARGETARCH
COPY toolset-runtime-linux-${TARGETARCH} /usr/local/bin/toolset-runtime
RUN chmod +x /usr/local/bin/toolset-runtime
ENTRYPOINT ["toolset-runtime"]

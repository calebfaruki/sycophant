FROM alpine:3.21

RUN apk add --no-cache ca-certificates git

COPY --from=ghcr.io/calebfaruki/transponder:latest \
    /usr/local/bin/transponder /usr/local/bin/
COPY --from=ghcr.io/calebfaruki/workspace-tools:latest \
    /usr/local/bin/workspace-tools /usr/local/bin/

RUN mkdir -p /workspace

ENTRYPOINT ["transponder"]

FROM alpine:3.21 AS fetch
ARG TARGETARCH
ARG CADDY_VERSION=2.11.7
ARG CADDY_SHA512_amd64=a7a433a1b133efc3c8d10eb0b99d52a24b5ef5c322dc77f5282182b1c0402139ab83f3a99f0c52409df77d20123fb0b523edad8a66d8f5e49136197bf61ef0e7
ARG CADDY_SHA512_arm64=3db36ba90c7a6e8dda40ee3dd71fa08844c76b5fb08f61b31e5e78d2ed38e71c51dc7baed875e50d1ca1279196e84302967237386ae87c91ae9f2aaceada682e
RUN set -eu; \
    case "${TARGETARCH}" in \
      amd64) sum="${CADDY_SHA512_amd64}" ;; \
      arm64) sum="${CADDY_SHA512_arm64}" ;; \
      *) echo "unsupported arch ${TARGETARCH}" >&2; exit 1 ;; \
    esac; \
    tarball="caddy_${CADDY_VERSION}_linux_${TARGETARCH}.tar.gz"; \
    wget -q -O "/tmp/${tarball}" "https://github.com/caddyserver/caddy/releases/download/v${CADDY_VERSION}/${tarball}"; \
    echo "${sum}  /tmp/${tarball}" | sha512sum -c -; \
    tar -xzf "/tmp/${tarball}" -C /tmp caddy

FROM alpine:3.21
COPY --from=fetch /tmp/caddy /usr/bin/caddy
USER 65532:65532

FROM alpine:3.21
ARG TARGETARCH
ARG KUBECTL_VERSION=v1.31.0
RUN apk add --no-cache curl ca-certificates && \
    curl -fsSL -o /usr/local/bin/kubectl \
      https://dl.k8s.io/release/${KUBECTL_VERSION}/bin/linux/${TARGETARCH}/kubectl && \
    chmod +x /usr/local/bin/kubectl && \
    apk del curl

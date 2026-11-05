# In-cluster helm runner. Ships the helm binary and a baked copy of each
# component chart at /charts/sycophant-<kind>, so a Job can run
# `helm upgrade --install <release> /charts/sycophant-<kind>` with no chart
# registry and no network fetch of the chart. It keeps curl: the instructions
# jobs write the store's filer with it (BusyBox wget sends no PUT or DELETE).
# Build context is the repo root (the charts live under charts/), e.g.
#   docker build -f images/provisioner/Dockerfile -t sycophant-provisioner:local .
FROM alpine:3.21
ARG TARGETARCH
ARG HELM_VERSION=v3.17.3
ARG KUBECTL_VERSION=v1.31.0
RUN apk add --no-cache curl ca-certificates jq && \
    curl -fsSL "https://get.helm.sh/helm-${HELM_VERSION}-linux-${TARGETARCH}.tar.gz" \
      | tar -xzO "linux-${TARGETARCH}/helm" > /usr/local/bin/helm && \
    chmod +x /usr/local/bin/helm && \
    curl -fsSL -o /usr/local/bin/kubectl \
      "https://dl.k8s.io/release/${KUBECTL_VERSION}/bin/linux/${TARGETARCH}/kubectl" && \
    chmod +x /usr/local/bin/kubectl
COPY charts/sycophant-tenant /charts/sycophant-tenant
COPY charts/sycophant-agent /charts/sycophant-agent
COPY charts/sycophant-inference /charts/sycophant-inference
COPY charts/sycophant-toolset /charts/sycophant-toolset
COPY charts/sycophant-user /charts/sycophant-user
COPY charts/sycophant-channel /charts/sycophant-channel
USER 65532:65532

# Hello World

Single workspace running the simple AGENTS.md fixture. Demonstrates the minimum surface: one principal-authored system prompt, one transponder pod, one chamber.

## Prerequisites

- Kubernetes cluster with Cilium CNI and the gVisor RuntimeClass
- `kubectl`, `helm`, `grpcurl` installed
- An LLM API key (Anthropic, Mistral, or OpenAI) — examples below use Anthropic

## Stage Mainframe content

The workspace reads `/etc/kernel/AGENTS.md` at startup. With `kernel.kind: HostPath`, the chart mounts the host directory at `/etc/kernel` directly (read-only). For local self-host on k3d (the supported runtime — see [docs/mainframe.md](../../../docs/mainframe.md)), the cluster sees the path on your machine directly. Author the fixture in your editor:

```sh
mkdir -p ~/sycophant/tmp/hello-world-data
cp examples/mainframe/simple/AGENTS.md \
  ~/sycophant/tmp/hello-world-data/AGENTS.md
```

For external S3, swap `kernel.kind: HostPath` for `kernel.kind: S3` with an `s3:` block pointing at your endpoint.

## Deploy

```sh
kubectl create namespace hello-world --dry-run=client -o yaml | kubectl apply -f -

kubectl create secret generic sycophant-llm-anthropic \
  --namespace hello-world \
  --from-literal=api-key="$ANTHROPIC_API_KEY" \
  --dry-run=client -o yaml | kubectl apply -f -

helm upgrade --install hello-world charts/sycophant-tenant/ \
  -n hello-world \
  -f examples/scenarios/hello-world/values.yaml \
  --wait
```

## Send a message

```sh
kubectl port-forward -n hello-world svc/tightbeam-ctrl 9090:9090 &
sleep 2

grpcurl -plaintext -d '{"register":{"channel_type":"test","channel_name":"hello","workspace":"hello-world"}}
{"user_message":{"content":[{"text":{"text":"Say hello"}}],"sender":"user"}}' \
  localhost:9090 tightbeam.v1.TightbeamController/ChannelStream

kill %1
```

## Teardown

```sh
helm uninstall hello-world -n hello-world
kubectl delete namespace hello-world
```

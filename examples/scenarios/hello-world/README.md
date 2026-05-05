# Hello World

Single workspace running the simple ENTRYPOINT.md fixture. Demonstrates the minimum surface: one principal-authored system prompt, one workspace pod, one chamber.

## Prerequisites

- Kubernetes cluster with Cilium CNI and the agent-sandbox controller
- `kubectl`, `helm`, `grpcurl` installed
- An LLM API key (Anthropic, Mistral, or OpenAI) — examples below use Anthropic

## Stage Mainframe content

The workspace reads `/etc/mainframe/ENTRYPOINT.md` at startup. The chart provisions a per-workspace Versitygw against the path you give it; Versitygw's posix backend treats the directory `instructions/` inside that path as the bucket.

For local self-host on k3d (the supported runtime — see [docs/mainframe.md](../../../docs/mainframe.md) for the runtime requirement), the cluster sees the path on your machine directly. Author the fixture in your editor:

```sh
mkdir -p ~/sycophant/tmp/hello-world-data/instructions
cp examples/mainframe/simple/ENTRYPOINT.md \
  ~/sycophant/tmp/hello-world-data/instructions/ENTRYPOINT.md
```

For external S3, replace the `instructions:` string with an object form pointing at your endpoint.

## Deploy

```sh
kubectl create namespace hello-world --dry-run=client -o yaml | kubectl apply -f -

kubectl create secret generic sycophant-llm-anthropic \
  --namespace hello-world \
  --from-literal=api-key="$ANTHROPIC_API_KEY" \
  --dry-run=client -o yaml | kubectl apply -f -

helm upgrade --install hello-world charts/sycophant/ \
  -n hello-world \
  -f examples/scenarios/hello-world/values.yaml \
  --set workspaces.hello-world.instructions=$HOME/sycophant/tmp/hello-world-data \
  --wait
```

## Send a message

```sh
kubectl port-forward -n hello-world svc/tightbeam-controller 9090:9090 &
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

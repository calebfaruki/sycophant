# Hello World

Single workspace running the simple ENTRYPOINT.md fixture. Demonstrates the minimum surface: one principal-authored system prompt, one workspace pod, one chamber.

## Prerequisites

- Kubernetes cluster with Cilium CNI and the agent-sandbox controller
- `kubectl`, `helm`, `grpcurl` installed
- An LLM API key (Anthropic, Mistral, or OpenAI) — examples below use Anthropic

## Stage Mainframe content

The workspace reads `/etc/mainframe/ENTRYPOINT.md` at startup. For a Kind/Docker Desktop cluster, copy the simple fixture onto the node:

```sh
docker exec desktop-control-plane mkdir -p /var/lib/sycophant/mainframe
docker cp examples/mainframe/simple/ENTRYPOINT.md \
  desktop-control-plane:/var/lib/sycophant/mainframe/ENTRYPOINT.md
```

For a managed cluster, place the file at whatever node path matches `mainframe.local.hostPath` (or use the git adapter when it lands).

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
  --set mainframe.local.hostPath=/var/lib/sycophant/mainframe \
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

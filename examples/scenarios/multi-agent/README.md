# Multi-Agent

Single workspace running an orchestrator ENTRYPOINT.md that delegates to two personas — Alice (warm, creative) and Bob (dry, technical) — via `llm_call`. Demonstrates 007's pattern: multi-agent behavior is principal-authored prose, not a system primitive.

## Prerequisites

- Kubernetes cluster with Cilium CNI and the agent-sandbox controller
- `kubectl`, `helm`, `grpcurl` installed
- An LLM API key — examples below use Anthropic

## Stage Mainframe content

The fixture at `examples/mainframe/orchestrator/` contains:

- `ENTRYPOINT.md` — the orchestrator. Reads the chosen delegate's system prompt and dispatches `llm_call`.
- `agents/alice/system_prompt.md`, `agents/bob/system_prompt.md` — the delegate personas.

Copy the whole tree onto the cluster node:

```sh
docker exec desktop-control-plane mkdir -p /var/lib/sycophant/multi-agent-mainframe
docker cp examples/mainframe/orchestrator/. \
  desktop-control-plane:/var/lib/sycophant/multi-agent-mainframe/
```

## Deploy

```sh
kubectl create namespace multi-agent --dry-run=client -o yaml | kubectl apply -f -

kubectl create secret generic sycophant-llm-anthropic \
  --namespace multi-agent \
  --from-literal=api-key="$ANTHROPIC_API_KEY" \
  --dry-run=client -o yaml | kubectl apply -f -

helm upgrade --install multi-agent charts/sycophant/ \
  -n multi-agent \
  -f examples/scenarios/multi-agent/values.yaml \
  --set mainframe.local.hostPath=/var/lib/sycophant/multi-agent-mainframe \
  --wait
```

## Send messages

The orchestrator picks the delegate per message based on tone/domain:

```sh
kubectl port-forward -n multi-agent svc/tightbeam-controller 9090:9090 &
sleep 2

# Creative — orchestrator should delegate to Alice
grpcurl -plaintext -d '{"register":{"channel_type":"test","channel_name":"chat","workspace":"multi-agent"}}
{"user_message":{"content":[{"text":{"text":"Help me come up with a name for my startup"}}],"sender":"user"}}' \
  localhost:9090 tightbeam.v1.TightbeamController/ChannelStream

# Technical — orchestrator should delegate to Bob
grpcurl -plaintext -d '{"register":{"channel_type":"test","channel_name":"chat2","workspace":"multi-agent"}}
{"user_message":{"content":[{"text":{"text":"Explain how TCP backpressure works"}}],"sender":"user"}}' \
  localhost:9090 tightbeam.v1.TightbeamController/ChannelStream

kill %1
```

Inspect the conversation log to see which delegate fired (entries tagged `delegate:<id>` in `conversation.ndjson`):

```sh
kubectl exec -n multi-agent multi-agent -c workspace-tools -- \
  grep '"role":"assistant"' /var/log/conversation/conversation.ndjson | tail -4
```

## Teardown

```sh
helm uninstall multi-agent -n multi-agent
kubectl delete namespace multi-agent
```

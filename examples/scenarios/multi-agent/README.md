# Multi-Agent

Deploy a multi-agent workspace with automatic routing. Messages are dispatched to Alice or Bob based on who's a better fit.

Alice is warm, enthusiastic, and creative. Bob is dry, precise, and technical. The transponder's router agent reads each message and decides who should handle it.

## Prerequisites

- Kubernetes cluster with Cilium CNI
- `kubectl`, `helm`, `grpcurl` installed
- `ANTHROPIC_API_KEY` set in environment

## Deploy

```sh
kubectl create namespace multi-agent --dry-run=client -o yaml | kubectl apply -f -

kubectl create configmap sycophant-prompt-alice \
  --namespace multi-agent \
  --from-file=examples/prompts/alice/ \
  --dry-run=client -o yaml | kubectl apply -f -

kubectl create configmap sycophant-prompt-bob \
  --namespace multi-agent \
  --from-file=examples/prompts/bob/ \
  --dry-run=client -o yaml | kubectl apply -f -

helm upgrade --install multi-agent charts/sycophant/ \
  -n multi-agent \
  -f examples/scenarios/multi-agent/values.yaml \
  --wait

kubectl create secret generic sycophant-llm-anthropic \
  --namespace multi-agent \
  --from-literal=api-key="$ANTHROPIC_API_KEY" \
  --dry-run=client -o yaml | kubectl apply -f -
```

Prompt ConfigMaps are created from prompt directories before helm install.
The router ConfigMap and TightbeamModel CRDs are rendered by Helm.

## Send messages

Try different types of questions and watch the router pick the right agent:

```sh
kubectl port-forward -n multi-agent svc/sycophant-controller 9090:9090 &
sleep 2

# Creative question — should route to Alice
grpcurl -plaintext -d '{"register":{"channel_type":"test","channel_name":"chat"}}
{"user_message":{"content":[{"text":{"text":"Help me come up with a name for my startup"}}],"sender":"user"}}' \
  localhost:9090 tightbeam.v1.TightbeamController/ChannelStream

# Technical question — should route to Bob
grpcurl -plaintext -d '{"register":{"channel_type":"test","channel_name":"chat2"}}
{"user_message":{"content":[{"text":{"text":"Explain how TCP backpressure works"}}],"sender":"user"}}' \
  localhost:9090 tightbeam.v1.TightbeamController/ChannelStream

kill %1
```

Check which agent handled each message:

```sh
kubectl logs -n multi-agent deployment/multi-agent -c transponder
```

## Teardown

```sh
helm uninstall multi-agent -n multi-agent
kubectl delete namespace multi-agent
```

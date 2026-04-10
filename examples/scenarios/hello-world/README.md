# Hello World

Deploy a single-agent workspace with one tool.

## Prerequisites

- Kubernetes cluster with Cilium CNI
- `kubectl`, `helm`, `grpcurl` installed
- `ANTHROPIC_API_KEY` set in environment

## Deploy

```sh
kubectl create namespace hello-world --dry-run=client -o yaml | kubectl apply -f -

kubectl create configmap sycophant-prompt-hello-world \
  --namespace hello-world \
  --from-file=examples/prompts/hello-world/ \
  --dry-run=client -o yaml | kubectl apply -f -

helm upgrade --install hello-world charts/sycophant/ \
  -n hello-world \
  -f examples/scenarios/hello-world/values.yaml \
  --wait

kubectl create secret generic sycophant-llm-anthropic \
  --namespace hello-world \
  --from-literal=api-key="$ANTHROPIC_API_KEY" \
  --dry-run=client -o yaml | kubectl apply -f -
```

Prompt ConfigMaps are created from prompt directories before helm install.
TightbeamModel CRDs and other resources are rendered by Helm.

## Send a message

```sh
kubectl port-forward -n hello-world svc/tightbeam-controller 9090:9090 &
sleep 2

grpcurl -plaintext -d '{"register":{"channel_type":"test","channel_name":"hello"}}
{"user_message":{"content":[{"text":{"text":"Say hello"}}],"sender":"user"}}' \
  localhost:9090 tightbeam.v1.TightbeamController/ChannelStream

kill %1
```

## Teardown

```sh
helm uninstall hello-world -n hello-world
kubectl delete namespace hello-world
```

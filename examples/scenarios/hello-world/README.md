# Hello World

Deploy a single-agent workspace with one tool.

## Prerequisites

- Kubernetes cluster with Cilium CNI
- `kubectl`, `helm`, `grpcurl` installed
- `ANTHROPIC_API_KEY` set in environment

## Deploy

```sh
kubectl create namespace hello-world --dry-run=client -o yaml | kubectl apply -f -

kubectl create configmap sycophant-agent-hello-world-hello-world \
  --namespace hello-world \
  --from-file=examples/agents/hello-world/ \
  --dry-run=client -o yaml | kubectl apply -f -

helm upgrade --install hello-world charts/sycophant/ \
  -n hello-world \
  -f examples/scenarios/hello-world/values.yaml \
  --wait

cat <<EOF > /tmp/sycophant-llm.env
provider=anthropic
model=claude-sonnet-4-20250514
api-key=${ANTHROPIC_API_KEY}
max-tokens=8192
EOF

kubectl create secret generic sycophant-llm-anthropic \
  --namespace hello-world \
  --from-env-file=/tmp/sycophant-llm.env \
  --dry-run=client -o yaml | kubectl apply -f -

rm /tmp/sycophant-llm.env
```

## Send a message

```sh
kubectl port-forward -n hello-world svc/sycophant-controller 9090:9090 &
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

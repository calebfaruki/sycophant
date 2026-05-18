# SSH Credentials (secret-scrubbing fixture)

Single-purpose scenario that exercises the transponder's secret-scrubber. The workspace loads the `ssh-credentials` chamber (see `examples/chambers/ssh-credentials/`), which exposes one tool — `test-cmd` — that emits the chamber-mounted SSH private key bytes to stdout. When the LLM invokes `test-cmd`, the result flows back through the transponder, where the scrubber replaces the key bytes with `[REDACTED:demo-ssh-key]` before the result lands in the conversation log.

The chamber's tool is intentionally trivial and benign-sounding so the LLM will invoke it on request without safety-refusing. The point is the path, not the payload.

## Prerequisites

- Kubernetes cluster + the cluster chart installed
- `kubectl`, `helm`, `grpcurl`
- An LLM API key (Anthropic or Mistral)
- The `airlock-ssh-credentials` chamber image pushed to a registry the cluster can reach (built from `examples/chambers/ssh-credentials/`)

## Stage Mainframe content

```sh
mkdir -p ~/sycophant/tmp/ssh-credentials-data
cp examples/mainframe/simple/AGENTS.md \
  ~/sycophant/tmp/ssh-credentials-data/AGENTS.md
```

## Deploy

```sh
kubectl create namespace ssh-credentials --dry-run=client -o yaml | kubectl apply -f -

kubectl apply -f examples/chambers/ssh-credentials/fixtures/ -n ssh-credentials

kubectl create secret generic sycophant-llm-anthropic \
  --namespace ssh-credentials \
  --from-literal=api-key="$ANTHROPIC_API_KEY" \
  --dry-run=client -o yaml | kubectl apply -f -

helm upgrade --install ssh-credentials charts/sycophant-tenant/ \
  -n ssh-credentials \
  -f examples/scenarios/ssh-credentials/values.yaml \
  --wait
```

## Trigger the tool

The prompt is intentionally minimal — `test-cmd` is a tool the LLM has on its menu, and the chat just asks for it:

```sh
kubectl port-forward -n ssh-credentials svc/tightbeam-ctrl 9090:9090 &
sleep 2

grpcurl -plaintext -d '{"register":{"channel_type":"test","channel_name":"scrub","workspace":"ssh-credentials"}}
{"user_message":{"content":[{"text":{"text":"Use the test-cmd tool."}}],"sender":"user"}}' \
  localhost:9090 tightbeam.v1.TightbeamController/ChannelStream

kill %1
```

## Assertions

1. Tool executed successfully — airlock-ctrl logged an `exit_code=0` tool result:
   ```sh
   kubectl logs -n ssh-credentials deployment/airlock-ctrl | grep 'received tool result.*exit_code=0'
   ```
2. Secret value did NOT leak into the transponder log — scrubber redacted before logging:
   ```sh
   kubectl logs -n ssh-credentials ssh-credentials -c transponder | grep -c 'FAKE-ED25519-PRIVATE-KEY'
   # expect: 0
   ```

## Teardown

```sh
helm uninstall ssh-credentials -n ssh-credentials
kubectl delete namespace ssh-credentials
```

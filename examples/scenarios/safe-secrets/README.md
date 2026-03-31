# Safe Secrets

Proves that credentials injected into airlock chambers never appear in agent output. The `echo-secret` tool runs `printenv SECRET_TOKEN` inside a chamber with a mounted secret. The airlock runtime scrubs the raw value from the output, replacing it with `[REDACTED:demo-secret]`.

Layer this on top of the [hello-world](../hello-world/) example.

## Prerequisites

- Running hello-world deployment (see [hello-world README](../hello-world/README.md))
- Or any sycophant deployment with a workspace named `hello-world`

## Deploy

Layer the safe-secrets values on top of hello-world:

```sh
helm upgrade --install hello-world charts/sycophant/ \
  -n hello-world \
  -f examples/scenarios/hello-world/values.yaml \
  -f examples/scenarios/safe-secrets/values.yaml \
  --wait

kubectl apply -f examples/scenarios/safe-secrets/fixtures/ -n hello-world
```

## Send a message

```sh
kubectl port-forward -n hello-world svc/sycophant-controller 9090:9090 &
sleep 2

grpcurl -plaintext -d '{"register":{"channel_type":"test","channel_name":"secrets"}}
{"user_message":{"content":[{"text":{"text":"Use the echo-secret tool"}}],"sender":"user"}}' \
  localhost:9090 tightbeam.v1.TightbeamController/ChannelStream

kill %1
```

## Verify

The raw secret should not appear anywhere in the logs:

```sh
kubectl logs -n hello-world deployment/hello-world -c transponder | grep -c "super-secret-value-12345"
```

Expected: 0.

## Teardown

Re-deploy without the safe-secrets layer, or tear down the full stack:

```sh
helm uninstall hello-world -n hello-world
kubectl delete namespace hello-world
```

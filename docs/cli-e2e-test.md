# CLI End-to-End Test Guide

Test the syco CLI with a local init in a temp directory.

## Prerequisites

- Docker Desktop with Kubernetes enabled
- `kubectl`, `helm` installed
- `ANTHROPIC_API_KEY` set in environment
- syco binary built: `cargo build -p syco`

## Step 1: Initialize

```sh
export SYCO=$(pwd)/target/debug/syco
cd /tmp && rm -rf syco-e2e && mkdir syco-e2e && cd syco-e2e

$SYCO init local e2e-test
```

Expected:
```
Checking Docker... ok
Checking Kubernetes... ok
Checking Helm... ok
Initialized in current directory (release: e2e-test).
```

Verify scaffolded files:
```sh
cat release        # e2e-test
cat values.yaml    # scaffold template
ls charts/sycophant/templates/
ls examples/
```

## Step 2: Configure a model

```sh
$SYCO model set haiku \
  --format anthropic \
  --model claude-haiku-4-5-20251001 \
  --base-url https://api.anthropic.com/v1 \
  --secret sycophant-llm-anthropic \
  --secret-env API_KEY
```

Expected: `Model 'haiku' configured.`

Verify values.yaml:
```sh
$SYCO model list
```

Expected:
```
NAME             FORMAT       MODEL                            URL
haiku            anthropic    claude-haiku-4-5-20251001         https://api.anthropic.com/v1
```

## Step 3: Deploy

Rewrite values.yaml with local images and add a workspace/agent:

```sh
cat > values.yaml << 'EOF'
controller:
  image: tightbeam-controller
  tag: local
  pullPolicy: Never
  llmJobImage: tightbeam-llm-job:local

airlock:
  image: airlock-controller
  tag: local
  pullPolicy: Never

transponder:
  image: sycophant-transponder
  tag: local
  pullPolicy: Never

models:
  haiku:
    format: anthropic
    model: claude-haiku-4-5-20251001
    baseUrl: https://api.anthropic.com/v1
    secret:
      name: sycophant-llm-anthropic
      env: API_KEY

agents:
  hello:
    model: haiku
    prompt:
      path: examples/prompts/hello-world

workspaces:
  demo:
    image: sycophant-workspace-tools
    tag: local
    pullPolicy: Never
    agents:
      - hello
EOF

kubectl create configmap sycophant-prompt-hello \
  --namespace e2e-test \
  --from-file=examples/prompts/hello-world/ \
  --dry-run=client -o yaml | kubectl apply -f -

kubectl create secret generic sycophant-llm-anthropic \
  --namespace e2e-test \
  --from-literal=api-key="$ANTHROPIC_API_KEY" \
  --dry-run=client -o yaml | kubectl apply -f -

$SYCO up
```

Expected: helm output showing deployment.

## Step 4: Verify

```sh
kubectl get pods -n e2e-test
kubectl get tightbeammodels -n e2e-test
```

Expected: pods running, haiku model registered.

## Step 5: Teardown

```sh
$SYCO down
```

Expected: `Stopping e2e-test...` followed by helm uninstall output.

Verify idempotency:
```sh
$SYCO down
```

Expected: `Not running.`

## Step 6: Cleanup

```sh
kubectl delete namespace e2e-test 2>/dev/null
cd /tmp && rm -rf syco-e2e
```

# CLI End-to-End Test Guide

Test the syco CLI with a local init in a temp directory.

## Prerequisites

- Docker Desktop with Kubernetes enabled
- `kubectl`, `helm` installed
- `ANTHROPIC_API_KEY` set in environment
- Local images built and loaded into cluster (see `docs/e2e-test.md` Step 0)
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

## Step 2: Configure model, agent, workspace

```sh
$SYCO model set haiku \
  --format anthropic \
  --model claude-haiku-4-5-20251001 \
  --base-url https://api.anthropic.com/v1 \
  --secret sycophant-llm-anthropic \
  --secret-env API_KEY

$SYCO agent set hello \
  --model haiku \
  --prompt examples/prompts/hello-world

$SYCO workspace create demo --image sycophant-workspace-tools:local
```

Verify:
```sh
$SYCO model list
$SYCO agent list
$SYCO workspace list
$SYCO workspace show demo
```

## Step 3: Add image overrides, assign agent, create secret, deploy

The scaffold values.yaml has no controller/airlock/transponder
image config. Append local image overrides and assign the agent
to the workspace:

```sh
cat >> values.yaml << 'EOF'

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
EOF
```

Add the agent to the workspace (no `workspace add-agent` command
yet — edit values.yaml manually):

```sh
sed -i '' 's/agents: \[\]/agents:\n    - hello/' values.yaml
```

Create the API key secret and deploy:

```sh
echo "$ANTHROPIC_API_KEY" | $SYCO secret set sycophant-llm-anthropic

$SYCO up
```

Expected: `Prompt 'hello' applied.` then helm output.

## Step 4: Verify

```sh
kubectl get pods -n e2e-test
kubectl get tightbeammodels -n e2e-test
$SYCO workspace show demo
$SYCO secret list
```

Expected: pods running, haiku model registered, workspace shows
hello agent, secret listed.

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

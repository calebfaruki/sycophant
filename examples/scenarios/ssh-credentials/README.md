# SSH Credentials (secret-scrubbing fixture)

Exercises the harness's secret-scrubber end-to-end: provisioned with the
`syco` CLI, triggered from the Flutter client. The
workspace loads the `ssh-credentials` chamber (see
`examples/chambers/ssh-credentials/`), which exposes one tool — `test-cmd` —
that emits the chamber-mounted SSH private key to stdout. When the LLM invokes
it, the result flows back through the harness, where the scrubber replaces
the key bytes with `[REDACTED:demo-ssh-key]` before anything lands in the
conversation log.

The tool is intentionally trivial and benign-sounding so the LLM invokes it on
request without safety-refusing. The point is the path, not the payload.

This scenario's assertion is more specific than `syco tenant audit`'s generic
clauses: it proves *this* chamber-emitted secret was redacted, by name. The
generic clause sweep (gVisor, egress, credential isolation, …) is covered by
the [hello-world](../hello-world/README.md) runbook.

## Prerequisites

- A sycophant checkout, Docker running, `syco` on PATH.
- An LLM API key — examples use OpenRouter (`$OPENROUTER_API_KEY`).

## 1. Cluster

```sh
syco setup
```

## 2. Stage kernel content

The harness reads this content in-process from its per-workspace read-only
kernel volume — no separate kernel-serving pod.

```sh
mkdir -p ~/sycophant/tmp/ssh-credentials-data
cp examples/mainframe/simple/AGENTS.md ~/sycophant/tmp/ssh-credentials-data/AGENTS.md
```

## 3. Tenant content

The LLM credential, provider, and model:

```sh
printf '%s' "$OPENROUTER_API_KEY" | syco tenant secret set openrouter --ns ssh-credentials
syco tenant provider set openrouter --secret openrouter --ns ssh-credentials
syco tenant model set deepseek/deepseek-v4-flash \
  --provider openrouter --secret openrouter --ns ssh-credentials
```

The demo SSH key the scrubber must redact, and the chamber that mounts it:

```sh
printf 'FAKE-ED25519-PRIVATE-KEY-DO-NOT-USE' | \
  syco tenant secret set demo-ssh-key --ns ssh-credentials

syco tenant chamber set ssh-credentials \
  --image sycophant-registry:5000/airlock-ssh-credentials:latest \
  --credential secret=demo-ssh-key,file=/home/agent/.ssh/id_ed25519 \
  --ns ssh-credentials
```

## 4. Workspace + deploy

```sh
mkdir -p ~/.config/sycophant/tenants/ssh-credentials
cp examples/scenarios/ssh-credentials/values.yaml \
  ~/.config/sycophant/tenants/ssh-credentials/values.yaml
# edit kernel.hostPath.path to: $HOME/sycophant/tmp/ssh-credentials-data (absolute)

syco tenant up --ns ssh-credentials
```

## 5. Trigger the tool

The CLI provisions; a client sends the message. Authorize a device and read its
enrollment code:

```sh
syco tenant enrollment set my-phone --workspace ssh-credentials --ns ssh-credentials
kubectl get enrollment my-phone -n ssh-credentials -o jsonpath='{.status.enrollmentCode}'
```

Enroll the Flutter app with that code (see
[`docs/flutter-app.md`](../../../docs/flutter-app.md)), then from its chat screen
ask for the tool the LLM has on its menu:

> Use the test-cmd tool.

## 6. Assertion — the key was scrubbed

The fake key must NOT appear in the harness stdout; the scrubber redacts it
before logging. Expect `0`:

```sh
kubectl logs -n ssh-credentials deployment/ssh-credentials -c harness \
  | grep -c 'FAKE-ED25519-PRIVATE-KEY'
# expect: 0   (the bytes were replaced with [REDACTED:demo-ssh-key])
```

## Teardown

```sh
syco tenant remove --ns ssh-credentials
syco destroy
```

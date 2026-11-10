<p align="center">
  <img src="docs/logo.png" alt="Sycophant" />
</p>

<h2 align="center">
  The zero-trust framework for autonomous AI agents.
</h2>

<p align="center">
  <a href="https://scorecard.dev/viewer/?uri=github.com/calebfaruki/sycophant"><img src="https://api.scorecard.dev/projects/github.com/calebfaruki/sycophant/badge" alt="OpenSSF Scorecard"></a>
  <a href="https://deepwiki.com/calebfaruki/sycophant"><img src="https://deepwiki.com/badge.svg" alt="Ask DeepWiki"></a>
  <a href="https://www.rust-lang.org/"><img src="https://img.shields.io/badge/Made%20with-Rust-1f425f.svg" alt="Made with Rust"></a>
  <img src="https://img.shields.io/badge/version-0.1.0-orange.svg" alt="Version 0.1.0">
  <a href="LICENSE"><img src="https://img.shields.io/badge/License-AGPL_3.0-blue.svg" alt="License: AGPL-3.0"></a>
</p>

Sycophant hosts autonomous AI agents in a Kubernetes cluster built on least privilege, zero knowledge and defense in depth. It treats everything the agent produces as adversarial.

> **Pre-release and under active development.** Expect sharp edges.

## Why Sycophant

Most agent frameworks are built for capability, not containment. Isolation is something you bolt on, if you remember to.

Hosted platforms take the other extreme. The model, your data, and the agent all live on their infrastructure. You get controls, but they are settings on machines you do not own, backed by promises you have to trust.

Sycophant is neither. Your data lives on infrastructure you choose. The agent answers only to the people you grant, and to their bound chat handles. Containment is the default, not an add-on.

## What a compromised agent can't do

Assume the agent is fully subverted: prompt injection, a poisoned tool, a bad model. Kubernetes itself contains it. Each tenant is a namespace and each component is a pod. The agent still cannot:

- **Read a secret.** The agent asks the harness for a tool or model by name. The harness names the Secret in a Job spec and never reads it. Only the pod that makes the call holds the value.
- **Leak a secret through output.** Before tool or model output returns to the agent, every credential value in it becomes a redaction marker.
- **Call the network freely.** Cilium gives every pod a default-deny egress firewall. Each toolset reaches only the domains it declared.
- **Break out of a tool.** gVisor sandboxes the pods that execute agent tool code, so a container escape stops at the sandbox.
- **Use a privileged identity.** Tool pods have no RBAC. Each token a pod mounts works with one service only.
- **Rewrite its own history.** The conversation log lives on a volume only the harness mounts. Tool pods never see it.
- **Reach another tenant.** Each tenant has its own network, storage and identity, with no route between tenants.
- **Disarm enforcement.** Kyverno creates each tenant's RBAC when its namespace appears, and blocks edits to security resources from inside it.

Chainsaw integration tests check these rules on a live cluster. Each test tries a forbidden action and checks that the cluster refuses.

## Quickstart

Sycophant ships as Helm charts. There are two ways in.

**CLI: local, secure by default.** The `sycoadm` CLI stands up a local [k3d](https://k3d.io) cluster with the full capability stack ([see Requirements](#requirements)) already wired in, then installs the hello-world pieces in [`examples/sycoadm/`](examples/sycoadm/). You need Docker, k3d, Go, a Rust toolchain with the `aarch64-unknown-linux-musl` target, jq, helm, kubectl, protoc, cmake, a C compiler, openssl, htpasswd, grpcurl, Tailscale, and the GGUF model at `~/.cache/sycophant/weights/Qwen3.5-2B-Q4_K_M.gguf`.

```sh
make sycoadm syco
export PATH="$PWD/target:$PATH"

# Create the cluster. Prompts for an operator email and password; idempotent.
# Every `up` below prompts for that password and waits for its installer.
sycoadm setup

sycoadm tenant up -f examples/sycoadm/tenants/hello-world.yaml

# The model server and weights images the inference example names.
docker pull ghcr.io/ggml-org/llama.cpp:server@sha256:9f84380be42d6285a827629c809387349c3541aa8986f7536547ca33cc8dd47a
docker tag ghcr.io/ggml-org/llama.cpp:server@sha256:9f84380be42d6285a827629c809387349c3541aa8986f7536547ca33cc8dd47a llama-server:local
docker build -f build/weights.Dockerfile --build-arg GGUF=Qwen3.5-2B-Q4_K_M.gguf \
  -t weights:local ~/.cache/sycophant/weights

sycoadm inference up -f examples/sycoadm/inference/qwen3-5-2b.yaml
sycoadm toolset up -f examples/sycoadm/toolsets/hello-world/values.yaml

# The demo secret for toolset hello-world (the agent's
# `secret.hello-world.demo-ssh-key` key). Keep it on one line. The
# scrubber redacts single-line values.
printf '%s' 'hello-world-demo-secret-DO-NOT-USE' \
  | sycoadm tenant secret set demo-ssh-key --tenant hello-world

sycoadm agent up -f examples/sycoadm/agents/hello-world.yaml

sycoadm instructions up -f examples/sycoadm/instructions/hello-world.yaml

sycoadm channel up -f examples/sycoadm/channels/tailnet.yaml

# Join the tailnet. Needs sudo; once per machine.
sycoadm connect --tenant hello-world

# Point syco at the relay on the tailnet.
mkdir -p ~/.config/syco
sed 's/${TAILNET_RELAY_ADDR}/relay:9090/' examples/syco/config.toml > ~/.config/syco/config.toml

syco login --email e2e-operator@sycophant.local
sycoadm user up -f examples/sycoadm/users/alice.yaml \
  --set "user.subject=https://127.0.0.1:30443/dex#$(syco whoami | awk '/^Subject:/ {print $2}')"

# Ask the agent to use ShowKernel and to WebFetch https://example.com (it fails:
# the toolset has no egress). Ask it to use GetSecret on demo-ssh-key. The
# reply shows [REDACTED:demo-ssh-key].
syco chat

# Tear down one piece, or everything.
sycoadm agent down hello-world --tenant hello-world
sycoadm destroy
```

`sycoadm upgrade` never touches channels. To replace an old `app` channel after an upgrade, use `sycoadm channel down app --tenant <tenant>` and then the `channel up` above. The new channel joins the tailnet as a new node, and clients reconnect.

**A public address.** `sycoadm ingress up --domain <name> --vpn-tenant <tenant>` gives the cluster one public IPv4 address. It never touches the router or DNS. First point an A record for `<name>` at this home's public IPv4, with no AAAA record. The command prints the router forward, the Docker Desktop setting and an optional CAA record to set by hand, then waits for Enter. It then publishes 443 from k3d and deploys Caddy, which gets a Let's Encrypt certificate. Any failure, or Ctrl-C, undoes every change. On routers without NAT loopback, such as the Livebox, home devices need a local DNS or hosts entry from `<name>` to the LAN IPv4. `--vpn-tenant` is temporary. It points Caddy at that tenant's Headscale until the cluster's shared VPN lands. Set that tenant's `headscale.serverUrl` to `https://<name>` so devices join through it.

`sycoadm ingress down` removes what `up` added, whatever part is present, and prints what is yours to remove by hand.

**DevOps: your own cluster.** Install the charts with Helm. The `sycophant-quickstart` chart bundles the gVisor RuntimeClass and the cluster layer. Install the `kyverno-crds` chart and Kyverno first ([see Prerequisites](charts/sycophant-quickstart/README.md#prerequisites)):

```sh
kubectl apply -f charts/sycophant-cluster/system-ns.yaml

helm install sycophant charts/sycophant-quickstart \
  -n sycophant-system --wait
```

Already provide your own RuntimeClass? Skip the bundle and install the cluster layer alone:

```sh
helm install sycophant charts/sycophant-cluster \
  -n sycophant-system --wait
```

## Components

<p align="center">
  <img src="docs/architecture.svg" alt="Authenticated people and channels (person or channel tokens) reach a per-agent harness through the relay-controller gateway. The harness runs the agent loop, owns the conversation log, and reads its capability catalog and instructions from read-only volumes. It reaches three capabilities on separate paths: a warm local-inference Service it dials directly with no credential, and a remote-inference Job and a tool Job, each an ephemeral gVisor sandbox the harness creates with only its own Secret and its own network egress. Only tool Jobs mount the workspace volume. Credentials exist only inside those Jobs, never with the agent." width="840" />
</p>

| Component | Role |
| --- | --- |
| **Harness** | The agent runtime and broker, one per agent. It drives the agent loop, owns the conversation log, and reads its own instructions (AGENTS.md and skills) in-process from a read-only volume. It sends each tool call and remote model call to a gated Job that holds only that toolset's Secrets and egress domains. It dials a warm local model directly, with no credential. |
| **Relay** | The client gateway. Authenticated people and channels (person or channel tokens) dial in through it to reach their agent, and it relays messages to and from the harness. |
| **Toolset** | The tool runtime. It executes tool calls in a gVisor sandbox the harness creates. |

Built as a Rust monorepo on gRPC (tonic/prost), kube-rs, and Helm charts. The `sycoadm` CLI drives it. Images target Linux arm64 and amd64.

## Requirements

Sycophant works on any conformant Kubernetes cluster that has three capabilities. Each is a swappable role with a sensible default:

- **Sandboxed container runtime:** a `gvisor` RuntimeClass. It isolates the toolsets that execute agent tool code.
- **Network egress control:** Cilium (default-deny egress with an L7 DNS allowlist).
- **Admission policy engine:** Kyverno 3.9.x, with its CRDs.

`sycoadm setup` installs this default set on a local k3d cluster for you.

## Local development

`make` with no arguments lists every target. `make lint-shell` needs the shellcheck, shfmt and editorconfig-checker that `scripts/install-test-deps.sh` installs. The local target is a k3d cluster. `scripts/e2e.sh` is the end-to-end test: it builds a hello-world deployment and asserts the security clauses. The fixes below expect the tenant namespace (`tenant-<10hex>`) in `$NS`. With one tenant:

```sh
NS=$(kubectl get ns -l app.kubernetes.io/part-of=sycophant-tenant -o name | cut -d/ -f2)
```

- **macOS stalls on fresh binaries.** XProtect scans each new binary the first time it starts, and a large test binary can sit at 0% CPU for 10+ minutes while `syspolicyd` works. Wait it out. Killing it restarts the scan. Adding your terminal under System Settings → Privacy & Security → Developer Tools skips the scans.
- **Docker VM disk, not RAM, is the limit.** A near-full VM trips kubelet eviction and a Kyverno eviction storm that looks like a memory problem. Keep about 15 GB free. `sycoadm setup` refuses under 8 GB.
- **A rebuilt image doesn't replace the live one.** k3d's containerd caches by `name:tag`. Remove the old one before re-importing:

  ```sh
  docker exec k3d-sycophant-server-0 ctr -n k8s.io image rm docker.io/library/<image>:local
  k3d image import <image>:local --cluster sycophant
  kubectl rollout restart deployment/<deployment> -n "$NS"
  ```

- **Stale or broken conversation history.** A harness crash mid-tool-call can leave a log the model rejects (`tool_use without tool_result`). Send `/clear` in `syco` to empty the conversation.

Charts live under [`charts/`](charts/) (`sycophant-quickstart` is the install bundle). The [examples](examples/sycoadm/) show how to write each piece.

## Security

Security is structure, not configuration. Simplicity, not complexity. See [SECURITY.md](SECURITY.md) for reporting and [THREAT_MODEL.md](THREAT_MODEL.md) for the full model.

## Contributing

Contributions are welcome. Open an issue to discuss a change, or send a pull request. Start with the design docs under [`docs/`](docs/) to get oriented.

## License

[AGPL-3.0](LICENSE)

Logo by [Bullitt](https://www.yo-bullitt.com/).

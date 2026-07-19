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
  <img src="https://img.shields.io/badge/version-0.01-orange.svg" alt="Version 0.01">
  <a href="LICENSE"><img src="https://img.shields.io/badge/License-AGPL_3.0-blue.svg" alt="License: AGPL-3.0"></a>
</p>

Sycophant runs autonomous AI agents inside a Kubernetes cluster built on least privilege, zero knowledge, and defense in depth. Everything the agent produces is treated as adversarial.

> **Pre-release and under active development.** The architecture below is the design the project is built around. Expect sharp edges.

## What a compromised agent can't do

Assume the agent is fully subverted — prompt injection, a poisoned tool, a bad model. It still cannot:

- **Read a secret.** Model keys and tool credentials exist only inside short-lived jobs, for the seconds a call needs them.
- **Call the network freely.** The agent has no direct egress. Every outbound route is a broker with an explicit allowlist.
- **Break out of a tool.** Each tool runs in its own throwaway sandbox with only the credentials and network access that tool declared.
- **Rewrite its own history.** The conversation log is owned by a separate component the agent can't write to.
- **Reach another tenant.** Every tenant has its own network, storage, and identity, with no route from one tenant into another.

## Quickstart

Sycophant ships as Helm charts. The `syco` CLI tool runs the chart installation for a local, secure-by-default setup using [k3d](https://k3d.io) and a default sandbox, network, and policy stack (gVisor, Cilium, Kyverno).

```sh
syco setup
```

Already running Kubernetes? Install the charts directly. Provide the three cluster capabilities (your own tooling and values):

- Sandboxed runtime exposing a `gvisor` RuntimeClass (or Kata)
- Cilium — default-deny egress, L7 DNS
- Kyverno 3.5.x + CRDs

The `sycophant-quickstart` chart bundles the gVisor RuntimeClass and Kyverno CRDs with the cluster layer if you'd rather install them together. To install the cluster layer directly — assuming you manage the RuntimeClass and CRDs yourself:

```sh
kubectl apply -f charts/sycophant-cluster/system-ns.yaml
helm install sycophant charts/sycophant-cluster \
  -n sycophant-system --set policyEngine=kyverno --wait

# ...or install the bundle instead, to get the RuntimeClass + Kyverno CRDs too:
helm install sycophant charts/sycophant-quickstart \
  -n sycophant-system --set policyEngine=kyverno --wait
```

## Components

Five components, each with a single, well-defined job.

<p align="center">
  <img src="docs/architecture.svg" alt="Registered devices reach a per-workspace Transponder through the Tightbeam gateway. The Transponder brokers model, tool, and prompt access through Hangar, Airlock, and Mainframe, which spawn ephemeral, credential-scoped Jobs below a trust boundary — credentials exist only in those jobs, never with the agent." width="840" />
</p>

| Component | Role |
| --- | --- |
| **Transponder** | The harness — the agent runtime, one per workspace. Runs the agent loop and owns the conversation history. |
| **Tightbeam** | The client gateway. Registered devices dial in through it to reach their agent, and it relays messages to and from the transponder. |
| **Hangar** | The model broker. Calls model-provider APIs on the agent's behalf. |
| **Airlock** | The tool broker. Runs each tool in an isolated, throwaway sandbox. |
| **Mainframe** | The prompt broker. Injects each workspace's instructions, sub-agents, and skills into the agent runtime. |

Hangar, airlock, and mainframe are brokers: the agent asks by name, and the broker holds the credentials — and any outbound network access — needed to answer, so neither secrets nor arbitrary egress reach the agent. Under the hood, each is a namespace-scoped Kubernetes controller that runs the work in ephemeral, credential-scoped Jobs. Tightbeam is the inbound counterpart — registered clients reach their agent only through it. The transponder itself talks only to these in-cluster components; every route to the internet is a named broker with an explicit allowlist.

Built as a Rust monorepo on gRPC (tonic/prost), Kubernetes CRDs (kube-rs), and Helm charts. The `syco` CLI drives it. Images target Linux arm64 and amd64.

## Requirements

Sycophant runs on any conformant Kubernetes cluster and relies on three cluster capabilities, each a swappable role with a sensible default:

- **Sandboxed container runtime** — gVisor by default (Kata is a supported alternative); isolates the chambers that run agent-executed tool code
- **Network egress control** — Cilium (default-deny with an L7 DNS allowlist)
- **Admission policy engine** — Kyverno by default

`syco setup` installs this default set on a local k3d cluster for you. Running it needs a local toolchain: k3d, helm, kubectl, cargo (rustup), protoc, cmake, and a C compiler.

## Local development

The local target is a k3d cluster. To exercise the full stack end to end:

```sh
OPENROUTER_API_KEY=... scripts/e2e.sh
```

This spins up a clean k3d cluster, builds and loads all images, deploys the Helm charts, and runs the security assertions. Charts live under [`charts/`](charts/) (`sycophant-quickstart` is the install bundle); per-component design docs live under [`docs/`](docs/) — [airlock](docs/airlock.md), [hangar](docs/hangar.md), [mainframe](docs/mainframe.md), [tightbeam](docs/tightbeam.md), and [secrets providers](docs/secrets-providers.md).

## Security

Security is structure, not configuration; simplicity, not complexity. See [SECURITY.md](SECURITY.md) for reporting and [THREAT_MODEL.md](THREAT_MODEL.md) for the full model.

## Contributing

Contributions are welcome. Open an issue to discuss a change, or send a pull request — start with the design docs under [`docs/`](docs/) to get oriented.

## License

[AGPL-3.0](LICENSE)

Logo by [Bullitt](https://www.yo-bullitt.com/).

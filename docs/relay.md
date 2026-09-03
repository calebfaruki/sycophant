# Relay

[![made-with-rust](https://img.shields.io/badge/Made%20with-Rust-1f425f.svg)](https://www.rust-lang.org/)

Client gateway for agent workspaces. Relay is the single ingress: it authorizes callers against the grants table, verifies every inbound request's signature, relays user messages to the Harness, and relays the assistant reply and turn-state back to the client. It holds no LLM credentials, no conversation history, and no transport of its own. Each channel supplies its own transport through its own adapter Deployment.

## How It Works

One component, the **gateway controller**, one per workspace namespace. It:

- Serves three ports: the harness link (9090), the app port (9091, reached from the app channel's adapter), and the adapter port (9092, reached from platform adapters).
- Watches the `relay-grants` ConfigMap and reloads the authorization table live.
- Verifies the ECDSA-P256 signature envelope on every inbound request against the registered public key of the grant row that signed it.
- Registers a presented public key against the grant row whose code was presented (`RedeemCode`).
- Holds the registered device public key of each grant row, and verifies every inbound signature against it.
- Relays inbound user messages → Harness, and outbound (assistant reply + turn-state) → client.

The gateway carries **no LLM credentials** and owns **no conversation log**. LLM dispatch is [Toolset](toolset.md); conversation history lives on the [Harness](harness.md). Relay is a relay across the trust boundary.

## Why Relay

The Harness and Toolset controller are in-cluster, trusted-network components. Something has to stand at the edge, authenticate external devices, and police what crosses in. That is Relay.

- **One authorization model, many transports** — authentication is pluggable per channel; authorization is central. Every request is checked against the same grants table, whatever transport carried it.
- **Row authorization** — a caller cannot talk to a workspace until a grant row names it. Adding a row invites; removing one revokes within seconds, with no pod restart.
- **Credential containment** — the gateway never holds LLM API keys; a compromised gateway cannot call providers. Its only Secret is the registered-key store, and every verb on it is name-scoped.

## Architecture

```
              adapter        gRPC :9091/:9092      gRPC :9090
   Client ───> Adapter ─────────────────> Gateway ───────> Harness
  (ECDSA-P256  (per-channel                (grants table,   (agent loop,
   signed)      transport)                  verify sig,      conversation
                                            registered keys) history)

   Client <─── Adapter <───────────────── Gateway <─────── Harness
  (assistant reply +                     (relay outbound)  (DeliverOutbound:
   turn-state)                                              reply + turn-state)
```

Inbound: the client signs a request, its channel's adapter delivers it, the gateway checks the signing grant row against the live grants table and verifies the signature against that row's registered key, then forwards user messages to the Harness. Outbound: the Harness originates the assistant reply and terminal turn-state and pushes them to the gateway (`DeliverOutbound`), which fans them out to the subscribed client.

## The two verification methods

How a channel's identity is proven. There are two, and the keypair granularity follows from which one a channel uses.

**Operator verification.** No registry exists behind the channel, so the operator is the vouching authority. The identity *is* the code: an unguessable string the operator invents, writes into the grant row, and hands over out of band. Possession of the string is the whole proof. The app channel is operator-verified.

*Granularity: one keypair per grant row, held by the enrolled client itself.* The private half lives in the device's platform secure storage and never enters the cluster; the relay stores only the public half, against that row. The row is spent once a key is registered, so revoke-and-re-invite is: delete the row, write a new row with a fresh string.

**Platform verification.** The platform's own authentication vouches — a Telegram login, an email passing DKIM, SPF, and DMARC — checked inside the adapter before any identity mapping. The identity is the platform handle. The operator writes the row; on first contact from that identity the adapter maps it to the row and speaks for it.

*Granularity: one keypair per adapter.* The adapter signs each envelope with its adapter key and asserts, per message, which grant row is speaking. The relay verifies the adapter signature, then checks the assertion against the grants table: the asserted row exists, and its channel matches the adapter that signed. Per-grant keys held by the adapter would add nothing — the adapter holds every key it would use.

Revocation is identical under both: the relay checks the asserted or signing row against the live grants table on every message, so deleting a row cuts access within seconds regardless of who holds which key.

Platform-verified channels do not take operator-invented codes. They would prove only what the platform already proves.

## The relay-grants ConfigMap

The relay's routing and authorization table. One ConfigMap named `relay-grants` per tenant namespace, one key per grant row:

```yaml
apiVersion: v1
kind: ConfigMap
metadata:
  name: relay-grants
  labels:
    sycophant.md/type: relay-grants
data:
  caleb-phone: |
    channel: app
    identity: kJ8f2QwXnR4tYv6b        # one-time code; spent after key registration
    workspace: family
  dad-telegram: |
    channel: telegram
    identity: "7133824091"            # Telegram numeric user id
    workspace: family
```

The operator is the only writer. Helm creates the object at install and never touches its `data` again, so an upgrade cannot stomp live rows. A Kyverno rule matching the `sycophant.md/type: relay-grants` label denies CREATE, UPDATE, and DELETE from every ServiceAccount inside the tenant namespace, the relay included.

Validation is invalid-is-absent, per row: known channel, non-empty identity, non-empty workspace, and no other field. A row that does not parse does not exist, and it never suppresses the rows beside it — a typo must not block a revocation made in the same edit. Rejected rows are logged and raised as Warning Events on the ConfigMap, so `kubectl describe configmap relay-grants` names the row and the reason.

Write a row with `kubectl`:

```bash
kubectl patch configmap relay-grants -n <namespace> --type=merge -p '{"data":{
  "caleb-laptop": "channel: app\nidentity: kJ8f2QwXnR4tYv6b\nworkspace: my-ws\n"
}}'
```

Revoke it by removing the key:

```bash
kubectl patch configmap relay-grants -n <namespace> --type=json \
  -p '[{"op":"remove","path":"/data/caleb-laptop"}]'
```

## Conversation Lifecycle

The conversation log lives on the **Harness**, on a dedicated per-workspace PVC (LocalFs). Relay exposes the conversation-lifecycle RPCs to clients but does not own them — it relays each to the Harness, which mints IDs, assembles history, persists turns, and answers reads.

- `MintConversation` — returns a new opaque `conversation_id` (a UUID). Per-workspace Harness routing is the isolation boundary; there is no `<workspace>.` prefix.
- `ListConversations`, `DeleteConversation`, `SetConversationName`, `GetConversationHistory` — relayed to the Harness.

The S3 conversation backend was dropped; the Harness persists to LocalFs only.

## gRPC Protocol

Proto definitions at `crates/relay-proto/proto/relay/v1/relay.proto`.

**`relay.v1.RelayGateway`** — the signature-verified, client-facing surface:

| RPC | Description |
|-----|-------------|
| `RedeemCode` | Present an operator-verified row's code; register the device public key against that row. |
| `ListWorkspaces` | The workspace the signing grant row names. |
| `ListGrants` | The workspace's grant menu, grouped by toolset. Names only, read from the mounted toolset bindings; the gateway reads no Secret. |
| `MintConversation` / `ListConversations` / `DeleteConversation` / `SetConversationName` / `GetConversationHistory` | Conversation lifecycle; relayed to the Harness. |
| `GetTurnState` | Current turn-state for a conversation. |
| `ChannelIngest` | Inbound user message in. |
| `ChannelReceive` | Server-stream of outbound events (assistant reply + turn-state) to the client. |
| `WatchTools` / `CallTool` | Tool list + invocation, relayed to the Harness. |

**`relay.v1.RelayInternal`** — the in-cluster surface the Harness calls back on:

| RPC | Description |
|-----|-------------|
| `Subscribe` | Harness subscribes to inbound user messages. |
| `SendServerNotification` / `SendServerRequestAndAwait` | Server-originated messages to the client. |
| `DeliverOutbound` | Harness pushes the assistant reply + terminal turn-state for fan-out to the client. |

## Channel adapters

The relay carries no transport. Each channel supplies its own, as a standing Deployment rendered from `.Values.channels`: one Deployment per entry, `replicas: 1`, `strategy: Recreate`, gVisor, its own ServiceAccount, its own fail-closed egress policy, and no workspace mount.

The app channel's adapter is the tailnet terminus. It is `sycophant.md/adapter-class: transport` — transport only, holding no signing identity and mounting no keypair volume — and forwards tailnet TCP :9090 to the relay's app port. Its tailnet node identity persists across restarts in its own `adapter-<channel>-state` Secret (`TS_KUBE_SECRET`). It is the stated exception to dial-out-only: it accepts inbound tailnet connections, so no ingress policy selects it. See [`docs/headscale-self-host-acme.md`](headscale-self-host-acme.md) for the self-hosted control plane.

Platform adapters (`sycophant.md/adapter-class: principal`) hold one adapter keypair each and reach the relay on the adapter port. They are dial-out-only.

## RBAC

The gateway ServiceAccount can read ConfigMaps, raise Events, read/write its own registered-key Secret, and authenticate caller SA tokens. It has **no Jobs, Deployments, or Pods RBAC**, no ConfigMap write verb, and no access to LLM credentials.

```yaml
rules:
  # The relay-grants ConfigMap: read-only. RBAC is the first lock on the
  # authorization table; the Kyverno rule is the second.
  - apiGroups: [""]
    resources: ["configmaps"]
    verbs: ["get", "list", "watch"]
  # Warning Events naming rejected grant rows.
  - apiGroups: ["events.k8s.io"]
    resources: ["events"]
    verbs: ["create", "patch"]
  # `create` cannot be name-scoped: `resourceNames` is not consulted on
  # create. A secret-name-allowlist VAP pins which Secret names this SA
  # may create. Needed for the first-ever redemption, which finds no
  # registered-keys Secret and creates one.
  - apiGroups: [""]
    resources: ["secrets"]
    verbs: ["create"]
  # Registered device keys, one per grant row. Name-scoped, because
  # `resourceNames` IS honored on update and patch — so no other Secret
  # in the namespace is reachable.
  - apiGroups: [""]
    resources: ["secrets"]
    resourceNames: ["relay-registered-keys"]
    verbs: ["get", "update", "patch"]
```

Each app adapter carries its own Role granting `get`/`update`/`patch` on its `adapter-<channel>-state` Secret (and `create` for first start), bound to the adapter's ServiceAccount, not the relay's.

TokenReview is cluster-scoped: a shared `cluster-relay-tokenreview` ClusterRole grants `create` on `tokenreviews`, bound to each tenant's `relay-ctrl` SA by the `tenant-rolebinding-generator` Kyverno policy.

## Security Model

- Every inbound request carries an ECDSA-P256 signature verified against the registered key of the grant row that signed it, and that row is re-checked against the live grants table on every request. A caller with no row cannot talk to the workspace.
- Conversations bind to the grant row that created them, not only to the workspace, so two rows in one workspace cannot read each other's conversations.
- The gateway holds no server signing key: it verifies client signatures and signs nothing. Its only Secret is the registered-key store, and every verb on it is name-scoped.
- The gateway holds no LLM credentials and no Jobs RBAC — it cannot dispatch LLM calls or call providers.
- The gateway owns no conversation log — history lives on the Harness's PVC, so a compromised gateway cannot forge or rewrite history.
- The secret-name-allowlist VAP pins which Secret names the gateway SA may create.
- All images are FROM scratch with musl static builds, signed with cosign (keyless, sigstore).

## Crate Structure

```
crates/
  relay-proto/        # gRPC proto definitions (relay.v1)
  relay-controller/   # gateway controller binary
```

## Installation

Container image is published to GHCR on each release:

```
ghcr.io/calebfaruki/relay-controller:latest
```

The per-tenant chart (`charts/sycophant-tenant/`) installs the gateway, the relay-grants ConfigMap, and one adapter Deployment per `.Values.channels` entry. Authorize a caller by writing its grant row (see the relay-grants ConfigMap section above). No custom kinds are on this path.

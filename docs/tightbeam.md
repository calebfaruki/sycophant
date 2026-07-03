# Tightbeam

[![made-with-rust](https://img.shields.io/badge/Made%20with-Rust-1f425f.svg)](https://www.rust-lang.org/)

Internet-facing client gateway for agent workspaces. Tightbeam is the single ingress: it authorizes devices, verifies every inbound request's signature, relays user messages to the Transponder, and relays the assistant reply and turn-state back to the client. It holds the per-tenant signing key and the tsnet bridge — but no LLM credentials and no conversation history.

## How It Works

One component, the **gateway controller**, one per workspace namespace. It:

- Terminates the external client connection over the tsnet bridge (a tailnet node).
- Verifies the ECDSA-P256 signature envelope on every inbound request against the device's enrolled public key.
- Mints and redeems enrollment codes (`Enrollment` CRD) to authorize new devices.
- Holds the per-tenant Ed25519 signing key for server identity.
- Relays inbound user messages → Transponder, and outbound (assistant reply + turn-state) → client.

The gateway carries **no LLM credentials** and owns **no conversation log**. LLM dispatch is [Hangar](hangar.md); conversation history lives on the [Transponder](transponder.md). Tightbeam is a relay across the trust boundary.

## Why Tightbeam

The Transponder and Hangar are in-cluster, trusted-network components. Something has to stand at the edge, authenticate external devices, and police what crosses in. That is Tightbeam.

- **One ingress, one auth model** — every external client reaches the workspace the same way: a tsnet-bridged connection with an ECDSA-P256-signed request envelope verified against an enrolled device key.
- **Device authorization** — a device cannot talk to a workspace until it redeems a one-time enrollment code and registers its public key.
- **Credential containment** — the gateway never holds LLM API keys; a compromised gateway cannot call providers. It holds only its own signing key, and cannot rotate it (no update/patch RBAC).

## Architecture

```
                tsnet bridge          gRPC
   Client ───────────────────> Gateway ───────> Transponder
  (ECDSA-P256                    (verify sig,     (agent loop,
   signed)                        enrollment,      conversation
                                  signing key)     history)

   Client <─────────────────── Gateway <─────── Transponder
  (assistant reply +          (relay outbound)   (DeliverOutbound:
   turn-state)                                    reply + turn-state)
```

Inbound: the client signs a request, the tsnet bridge delivers it, the gateway verifies the signature against the enrolled device key, and forwards user messages to the Transponder. Outbound: the Transponder originates the assistant reply and terminal turn-state and pushes them to the gateway (`DeliverOutbound`), which fans them out to the subscribed client.

## Enrollment CR

Device authorization. `Enrollment` (shortname `enr`) declares which workspaces a device may reach and records its registration.

```yaml
apiVersion: sycophant.md/v1
kind: Enrollment
metadata:
  name: caleb-laptop
  namespace: workspace-my-ws
spec:
  workspaces: ["my-ws"]          # workspaces this device may reach
status:
  enrollmentCode: "ABCD-1234"    # one-time code minted by the gateway
  publicKey: "<ECDSA-P256 SPKI>" # registered on redemption
  enrolledAt: "2026-06-30T12:00:00Z"
```

Lifecycle:

1. Operator creates an `Enrollment` with `spec.workspaces`. The gateway's enrollment watcher mints a one-time `status.enrollmentCode`.
2. The device redeems the code (`RedeemEnrollment`), submitting its ECDSA-P256 public key. The gateway records `status.publicKey` and `status.enrolledAt`.
3. Every subsequent request from that device carries a signature the gateway verifies against `status.publicKey`.

Manage via the CLI:

```bash
syco tenant enrollment set caleb-laptop --workspace my-ws
syco tenant enrollment list
syco tenant enrollment delete caleb-laptop
```

## Conversation Lifecycle

The conversation log lives on the **Transponder**, on a dedicated per-workspace PVC (LocalFs). Tightbeam exposes the conversation-lifecycle RPCs to clients but does not own them — it relays each to the Transponder, which mints IDs, assembles history, persists turns, and answers reads.

- `MintConversation` — returns a new opaque `conversation_id` (a UUID). Per-workspace Transponder routing is the isolation boundary; there is no `<workspace>.` prefix.
- `ListConversations`, `DeleteConversation`, `SetConversationName`, `GetConversationHistory` — relayed to the Transponder.

The S3 conversation backend was dropped; the Transponder persists to LocalFs only.

## gRPC Protocol

Proto definitions at `crates/tightbeam-proto/proto/tightbeam/v1/tightbeam.proto`.

**`tightbeam.v1.TightbeamGateway`** — the signature-verified, client-facing surface:

| RPC | Description |
|-----|-------------|
| `RedeemEnrollment` | Redeem a one-time code, register the device public key. |
| `ListWorkspaces` | Workspaces the enrolled device may reach. |
| `MintConversation` / `ListConversations` / `DeleteConversation` / `SetConversationName` / `GetConversationHistory` | Conversation lifecycle; relayed to the Transponder. |
| `GetTurnState` | Current turn-state for a conversation. |
| `ChannelIngest` | Inbound user message in. |
| `ChannelReceive` | Server-stream of outbound events (assistant reply + turn-state) to the client. |
| `WatchTools` / `CallTool` | Tool list + invocation, relayed to the Transponder. |

**`tightbeam.v1.TightbeamInternal`** — the in-cluster surface the Transponder calls back on:

| RPC | Description |
|-----|-------------|
| `Subscribe` | Transponder subscribes to inbound user messages. |
| `SendServerNotification` / `SendServerRequestAndAwait` | Server-originated messages to the client. |
| `DeliverOutbound` | Transponder pushes the assistant reply + terminal turn-state for fan-out to the client. |

## tsnet Bridge

The gateway reaches external clients over a tsnet node (a Tailscale-compatible tailnet), not a public Service. The bridge runs as a sidecar sharing the gateway Pod's ServiceAccount. Its tailnet node identity persists across restarts in the `tightbeam-tsnet-bridge-state` Secret (`TS_KUBE_SECRET`). See [`docs/headscale-self-host-acme.md`](headscale-self-host-acme.md) for the self-hosted control plane.

## RBAC

The gateway ServiceAccount can manage `Enrollment` CRs, read/write its own signing-key Secret, and authenticate caller SA tokens. It has **no Jobs RBAC** and no access to LLM credentials.

```yaml
rules:
  # Enrollment CRs: watch + list (mint one-time codes onto status);
  # get (redeem path); status patch (record redemption).
  - apiGroups: ["sycophant.md"]
    resources: ["enrollments"]
    verbs: ["get", "list", "watch"]
  - apiGroups: ["sycophant.md"]
    resources: ["enrollments/status"]
    verbs: ["patch"]
  # Signing-key bootstrap. The gateway mints the Ed25519 seed and stores
  # it in `tightbeam-signing-key` on first start; reads it on restart.
  # No update/patch — a compromised gateway cannot rotate the key. A
  # secret-name-allowlist VAP pins which Secret names this SA may create.
  - apiGroups: [""]
    resources: ["secrets"]
    verbs: ["create", "get"]
```

When the tsnet bridge is enabled, a second Role grants the SA `get`/`update`/`patch` on the `tightbeam-tsnet-bridge-state` Secret (and `create` for first start) so the sidecar can persist its tailnet identity.

TokenReview is cluster-scoped: a shared `cluster-tightbeam-tokenreview` ClusterRole grants `create` on `tokenreviews`, bound to each tenant's `tightbeam-ctrl` SA by the `tenant-rolebinding-generator` Kyverno policy.

## Security Model

- Every inbound request carries an ECDSA-P256 signature verified against an enrolled device key; unenrolled devices cannot talk to the workspace.
- The gateway holds only its own Ed25519 signing key, and cannot rotate it (no update/patch on the signing-key Secret).
- The gateway holds no LLM credentials and no Jobs RBAC — it cannot dispatch LLM calls or call providers.
- The gateway owns no conversation log — history lives on the Transponder's PVC, so a compromised gateway cannot forge or rewrite history.
- The secret-name-allowlist VAP pins which Secret names the gateway SA may create.
- All images are FROM scratch with musl static builds, signed with cosign (keyless, sigstore).

## Crate Structure

```
crates/
  tightbeam-proto/        # gRPC proto definitions (tightbeam.v1)
  tightbeam-controller/   # gateway controller binary
```

## Installation

Container image is published to GHCR on each release:

```
ghcr.io/calebfaruki/tightbeam-controller:latest
```

CRDs (`Enrollment`) ship in the cluster chart (`charts/sycophant-cluster/crds/`) and are installed once per cluster. The per-tenant chart (`charts/sycophant-tenant/`) installs the gateway in each workspace namespace. Enroll devices with `syco tenant enrollment set`.

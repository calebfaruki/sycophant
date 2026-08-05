# Sycophant Threat Model

**The pods that execute LLM-driven code (harness, llm-job, tool-job) are adversarial, and the rest of the cluster stays safe regardless of what they do.**

The harness pod hosts an autonomous LLM agent that processes untrusted inputs: user prompts, tool results, model outputs, retrieved documents. Any of those inputs can subvert the agent's behavior (prompt injection, poisoned tool output, model compromise). Once subverted, the agent acts with the full authority of the harness pod. **Therefore every privilege the harness pod carries is exploit surface for the agent itself.** The mitigation is structural: refuse to grant the harness anything that could be misused if the agent is subverted. This is the principle behind narrow single-audience tokens, `automountServiceAccountToken: false`, zero-RBAC workspace SA, and credentials the proxy names but never holds. Granting plural-audience tokens or broad RBAC anywhere on the harness surface defeats this mitigation.

A fully compromised workspace must not be able to:

0. **See secrets**: LLM API keys (hangar) and toolset tool credentials (toolset) are stored as Kubernetes Secrets. The workspace requests actions by name. The proxy references the credential Secret by name and never reads its plaintext. The kubelet mounts the Secret value only into the short-lived job pod that executes the call, and only while that call runs. For hangar, the key is a projected file at `/run/secrets/hangar/api-key`, never an env var. For toolset, it is an env var or a file, per the toolset's declaration. The plaintext never enters the proxy, and never enters workspace memory, env, or filesystem. **This is the load-bearing clause.**

1. **Exfiltrate**: reach network destinations its toolset didn't declare. This is enforced by a Cilium L7 DNS FQDN allowlist, egress default-deny on the harness, and per-component CiliumNetworkPolicies on tool-job and llm-job.

2. **Forge history**: write, hide, or rewrite conversation log entries. The harness is the sole author of conversation log entries. Its history PVC is separate from the toolset-mounted workspace PVC, so the workspace's runtime cannot write them directly.

3. **Impersonate**: present as a different workspace, tenant, or trusted in-cluster service. The mechanisms are audience-bound SA tokens (KEP-1205) with one audience per pair (harness→hangar, harness→toolset, harness→relay, llm-job→hangar), server-minted `channel_id`, and a P-256 `ClientSignatureVerifier` on the external surface.

4. **Escape**: break out of its sandbox to host kernel, other tenants' pods, or the cluster-control plane. gVisor (or operator-opted Kata) `runtimeClassName` is mandatory on the `tool-job` toolsets that run agent-executed tool code, enforced by the `cluster-gvisor-pod-policy` ValidatingAdmissionPolicy. The harness and llm-job run on the kubelet-default runtime with seccomp `RuntimeDefault` as the compensating control. All workspace pods share the same baseline: PSA restricted, perimeter CiliumNetworkPolicies, drop-`ALL` capabilities, `readOnlyRootFilesystem`, `runAsNonRoot`.

5. **Disarm**: tamper with the policy machinery (Kyverno, VAP, CiliumNetworkPolicies, RBAC) that enforces 0–4. The `cluster-protect-security` Kyverno ClusterPolicy denies same-namespace edits to CiliumNetworkPolicies, NetworkPolicies, RBAC, ServiceAccounts, and ConfigMaps. The cluster VAPs and their bindings live in `admissionregistration.k8s.io`, which Kyverno cannot guard. They are protected instead by RBAC absence: the zero-RBAC workspace runtime SA gets no verbs on `validatingadmissionpolicies`, `validatingadmissionpolicybindings`, `clusterpolicies`, `clusterroles`, `clusterrolebindings`, or `ciliumnetworkpolicies`.

## Beyond the core model: controller blast radius

The clauses above assume the controllers are trusted. Defense in depth bounds them anyway. The chief concern is the relay-controller, the one controller that terminates the external client surface and so is the most exposed:

- The `cluster-relay-secret-name-allowlist` ValidatingAdmissionPolicy bounds a compromised relay-controller to creating only its two named Secrets (`relay-signing-key`, `relay-tsnet-bridge-state`), even though its RBAC grants unconstrained `secrets: create` (Kubernetes ignores `resourceNames` on the `create` verb).
- Only the per-tenant `hangar-ctrl` / `toolset-ctrl` SAs may create `llm-job` / `tool-job`-labeled Jobs (`cluster-protect-security`), so no other in-namespace identity can spawn adversarial workloads under those labels.

## Why

Without clause 0, sycophant is "a sandboxed prompt-injection target that leaks the API key on first exploit." Hangar (LLM dispatch proxy) and toolset (tool dispatch proxy) are the entire reason the architecture exists. The harness reads its own kernel in-process from a read-only volume — no network hop, no proxy, and no added RBAC or Secret access. Clauses 1–5 protect clause 0 from being bypassed by any other route.

## How to apply

Any security or test-coverage discussion starts here. Lead with the thesis sentence, then the clauses, with 0 marked as load-bearing. Mechanisms (gVisor, VAP, CNP, audience binding, etc.) are never the headline. They are how a specific clause is enforced. Never present these as a flat list. The structure (one thesis → six clauses → mechanisms) is the point.

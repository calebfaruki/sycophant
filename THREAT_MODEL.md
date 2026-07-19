# Sycophant Threat Model

**The pods that execute LLM-driven code (transponder, llm-job, airlock-job) are adversarial; the rest of the cluster stays safe regardless of what they do.**

The transponder pod hosts an autonomous LLM agent that processes untrusted inputs — user prompts, tool results, model outputs, retrieved documents. Any of those inputs can subvert the agent's behavior (prompt injection, poisoned tool output, model compromise). Once subverted, the agent acts with the full authority of the transponder pod. **Therefore every privilege the transponder pod carries is exploit surface for the agent itself.** The mitigation is structural: refuse to grant the transponder anything that could be misused if the agent flips. This is the principle behind narrow single-audience tokens, `automountServiceAccountToken: false`, zero-RBAC workspace SA, and proxy-held credentials. Plural-audience tokens or broad RBAC anywhere on the transponder surface = security-architecture unravel.

A fully compromised workspace must not be able to:

0. **See secrets** — LLM API keys (hangar) and chamber tool credentials (airlock) live with the proxies. The workspace requests actions by name; proxies attach credentials and execute. Plaintext secrets never enter workspace memory, env, or filesystem. **This is the load-bearing clause.**

1. **Exfiltrate** — reach network destinations its chamber didn't declare. Cilium L7 DNS FQDN allowlist; egress default-deny on the transponder; per-component CiliumNetworkPolicies on airlock-job and llm-job.

2. **Forge history** — write, hide, or rewrite conversation log entries. The transponder is the sole author of conversation log entries; its history PVC is separate from the chamber-mounted workspace PVC, so the workspace's runtime cannot write them directly.

3. **Impersonate** — present as a different workspace, tenant, or trusted in-cluster service. Audience-bound SA tokens (KEP-1205) with one audience per pair (transponder→hangar, transponder→airlock, transponder→mainframe, transponder→tightbeam, llm-job→hangar), server-minted `channel_id`, P-256 `ClientSignatureVerifier` on the external surface.

4. **Escape** — break out of its sandbox to host kernel, other tenants' pods, or the cluster-control plane. gVisor (or operator-opted Kata) `runtimeClassName` is mandatory on the `airlock-job` chambers that run agent-executed tool code, enforced by the `cluster-gvisor-pod-policy` ValidatingAdmissionPolicy. The transponder and llm-job run on the kubelet-default runtime with seccomp `RuntimeDefault` as the compensating control. Universal envelope on all workspace pods: PSA restricted, perimeter CiliumNetworkPolicies, drop-`ALL` capabilities, `readOnlyRootFilesystem`, `runAsNonRoot`.

5. **Disarm** — tamper with the policy machinery (Kyverno, VAP, CiliumNetworkPolicies, RBAC) that enforces 0–4. The `cluster-protect-security` Kyverno ClusterPolicy denies same-namespace edits to CiliumNetworkPolicies, NetworkPolicies, RBAC, ServiceAccounts, and ConfigMaps. The cluster VAPs and their bindings live in `admissionregistration.k8s.io`, which Kyverno cannot guard; they are protected by RBAC absence — the zero-RBAC workspace runtime SA gets no verbs on `validatingadmissionpolicies`, `validatingadmissionpolicybindings`, `clusterpolicies`, `clusterroles`, `clusterrolebindings`, or `ciliumnetworkpolicies`.

## Beyond the core model: controller blast radius

The clauses above assume the controllers are trusted. Defense in depth bounds them anyway — chiefly the tightbeam-controller, the one controller that terminates the external client surface and so is the most exposed:

- The `cluster-tightbeam-secret-name-allowlist` ValidatingAdmissionPolicy bounds a compromised tightbeam-controller to creating only its two named Secrets (`tightbeam-signing-key`, `tightbeam-tsnet-bridge-state`), even though its RBAC grants unconstrained `secrets: create` (Kubernetes ignores `resourceNames` on the `create` verb).
- Only the per-tenant `hangar-ctrl` / `airlock-ctrl` SAs may create `llm-job` / `airlock-job`-labeled Jobs (`cluster-protect-security`), so no other in-namespace identity can spawn adversarial workloads under those labels.

## Why

Without clause 0, sycophant is "a sandboxed prompt-injection target that leaks the API key on first exploit." Hangar (LLM dispatch proxy) and airlock (tool dispatch proxy) — and mainframe-ctrl (prompt dispatch proxy) — are the entire reason the architecture exists. Clauses 1–5 protect clause 0 from being bypassed by any other route.

## How to apply

Any security or test-coverage discussion starts here. Lead with the thesis sentence, then the clauses, with 0 marked as load-bearing. Mechanisms (gVisor, VAP, CNP, audience binding, etc.) are never the headline — they're how a specific clause is enforced. Never present these as a flat list; the structure (one thesis → six clauses → mechanisms) is the point.

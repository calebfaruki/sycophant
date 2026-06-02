# Sycophant Threat Model

**The workspace is adversarial; the rest of the cluster stays safe regardless of what it does.**

The workspace runs an autonomous LLM agent that processes untrusted inputs — user prompts, tool results, model outputs, retrieved documents. Any of those inputs can subvert the agent's behavior (prompt injection, poisoned tool output, model compromise). Once subverted, the agent acts with the full authority of the workspace's runtime pod. **Therefore every privilege the workspace's runtime carries is exploit surface for the agent itself.** The mitigation is structural: refuse to grant the workspace anything that could be misused if the agent flips. This is the principle behind narrow single-audience tokens, `automountServiceAccountToken: false`, zero-RBAC workspace SA, and proxy-held credentials. Plural-audience tokens or broad RBAC anywhere on the workspace surface = security-architecture unravel.

A fully compromised workspace must not be able to:

0. **See secrets** — LLM API keys (tightbeam) and chamber tool credentials (airlock) live with the proxies. The workspace requests actions by name; proxies attach credentials and execute. Plaintext secrets never enter workspace memory, env, or filesystem. **This is the load-bearing clause.**

1. **Exfiltrate** — reach network destinations its chamber didn't declare. Cilium L7 DNS FQDN allowlist; egress default-deny on the transponder; per-component CiliumNetworkPolicies on mainframe-task, airlock-job, llm-job, channel-job.

2. **Forge history** — write, hide, or rewrite conversation log entries (and per ADR 018, memory entries too). Tightbeam mints conversation log entries server-side; mainframe-ctrl owns the memory store. The workspace's runtime cannot write either directly.

3. **Impersonate** — present as a different workspace, tenant, or trusted in-cluster service. Audience-bound SA tokens (KEP-1205) with one audience per pair (transponder→tightbeam, transponder→airlock, transponder→mainframe-ctrl, mainframe-ctrl→mainframe-task), server-minted `channel_id`, P-256 `ClientSignatureVerifier` on the external surface.

4. **Escape** — break out of its sandbox to host kernel, other tenants' pods, or the cluster-control plane. gVisor (or operator-opted Kata) `runtimeClassName` is mandatory on `transponder`, `mainframe-task`, `airlock-job`, `llm-job`, `channel-job`; enforced by `cluster-transponder-pod-policy` and `cluster-gvisor-pod-policy` ValidatingAdmissionPolicies; `runc` is forbidden. PSA restricted, perimeter CiliumNetworkPolicies, drop-`ALL` capabilities, `readOnlyRootFilesystem`, `runAsNonRoot`.

5. **Disarm** — tamper with the policy machinery (Kyverno, VAP, CiliumNetworkPolicies, RBAC) that enforces 0–4. `cluster-protect-security` ClusterPolicy + Kyverno pinned-by-digest + separate node pool. Tenant SAs never get `*verbs*` on `validatingadmissionpolicies`, `validatingadmissionpolicybindings`, `clusterpolicies`, `clusterroles`, `clusterrolebindings`, or `ciliumnetworkpolicies`.

## Why

Without clause 0, sycophant is "a sandboxed prompt-injection target that leaks the API key on first exploit." Tightbeam (LLM dispatch proxy) and airlock (tool dispatch proxy) — and per ADR 018, mainframe-ctrl (filesystem-tool dispatch proxy) — are the entire reason the architecture exists. Clauses 1–5 protect clause 0 from being bypassed by any other route.

## How to apply

Any security or test-coverage discussion starts here. Lead with the thesis sentence, then the clauses, with 0 marked as load-bearing. Mechanisms (gVisor, VAP, CNP, audience binding, etc.) are never the headline — they're how a specific clause is enforced. Never present these as a flat list; the structure (one thesis → six clauses → mechanisms) is the point.

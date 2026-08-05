{{/*
Guard for the three Kyverno ClusterPolicies. Encodes the policyEngine
decision once so the policy templates stay `{{- if include ... }}`.

  external → emit nothing (skip; operator supplies their own admission)
  kyverno  → CRDs present → emit "true" (render); absent → hard `fail`

Schema `required`+`enum` reject unset/invalid before templates run, so
only the two valid values reach here. `fail` fires on `include`
regardless of the enclosing `if`, so a Kyverno-less cluster errors loudly
instead of silently shipping without tenant-isolation policies.
*/}}
{{- define "sycophant.renderKyverno" -}}
{{- if eq .Values.policyEngine "kyverno" -}}
{{- if not (.Capabilities.APIVersions.Has "kyverno.io/v1/ClusterPolicy") -}}
{{- fail "policyEngine=kyverno but the Kyverno CRDs are absent (kyverno.io/v1/ClusterPolicy not found). Install the kyverno-crds chart (or run `syco setup`, which installs Kyverno) before installing sycophant-cluster; or set policyEngine=external to supply your own admission control." -}}
{{- end -}}
true
{{- end -}}
{{- end -}}

{{- define "sycophant.labels" -}}
app.kubernetes.io/managed-by: {{ .Release.Service }}
app.kubernetes.io/instance: {{ .Release.Name }}
app.kubernetes.io/part-of: sycophant
{{- end -}}

{{/*
Kyverno precondition: the rule applies only when the calling
ServiceAccount's namespace equals the request namespace. Scopes
tenant-operator writes to its own tenant; defends against a
cross-tenant RoleBinding leak.

`split(request.userInfo.username, ':') | [2]` pulls the namespace out
of `system:serviceaccount:NS:NAME`. Backticks escape the Helm template
so Kyverno's own templating fires at admission time, not at chart
render.
*/}}
{{- define "sycophant.kyvernoSameNamespacePrecondition" -}}
preconditions:
  all:
    - key: "{{`{{ split(request.userInfo.username, ':') | [2] || '' }}`}}"
      operator: Equals
      value: "{{`{{ request.namespace }}`}}"
{{- end -}}

{{/*
The sycophant.md CRD resources the per-tenant operator role manages.
Mirrored verbatim into `kyverno-rbac-aggregation.yaml` so kyverno's
RoleBinding generate rule passes K8s anti-escalation. Adding a CRD to
the framework requires updating ONLY this helper (and the
tenant-deployer ClusterRole separately — its verbs and resource set
differ).
*/}}
{{- define "sycophant.operatorCrds" -}}
- "toolsets"
- "clients"
- "models"
- "providers"
{{- end -}}

{{/*
Status subresources matching `sycophant.operatorCrds`. Kept in
parallel rather than computed from the base list so Helm rendering
stays trivially predictable; divergence risk is the same as the base
list (both staked on the same mirror invariant).
*/}}
{{- define "sycophant.operatorCrdStatuses" -}}
- "toolsets/status"
- "clients/status"
- "models/status"
- "providers/status"
{{- end -}}

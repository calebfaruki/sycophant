{{- define "sycophant.labels" -}}
app.kubernetes.io/managed-by: {{ .Release.Service }}
app.kubernetes.io/instance: {{ .Release.Name }}
app.kubernetes.io/part-of: sycophant
{{- end -}}

{{- define "sycophant.workspaceLabels" -}}
{{ include "sycophant.labels" .context }}
app.kubernetes.io/component: workspace
app.kubernetes.io/name: {{ .name }}
{{- end -}}

{{- define "sycophant.needsRouter" -}}
{{- $agents := .agents | default list -}}
{{- if gt (len $agents) 1 -}}
  {{- $hasCustom := false -}}
  {{- range $agent := $agents -}}
    {{- if eq $agent.name "router" -}}
      {{- $hasCustom = true -}}
    {{- end -}}
  {{- end -}}
  {{- if not $hasCustom -}}true{{- end -}}
{{- end -}}
{{- end -}}

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

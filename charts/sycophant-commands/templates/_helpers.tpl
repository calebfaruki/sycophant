{{- define "commands.tenantNs" -}}tenant-{{ . }}{{- end -}}

{{- define "commands.name" -}}
{{- printf "%s-%s-%s" .verb .kind (.target | toString | sha256sum | trunc 10) -}}
{{- end -}}

{{- /*
A command ConfigMap. Params: name, namespace, target (optional: the full target,
written to sycophant.md/target), data (a flat map; every value is written as a
quoted string, since command data is flat strings). YAML numbers arrive as
floats, so a whole one is printed as an integer, never as 1e+06.
*/ -}}
{{- define "commands.command" -}}
apiVersion: v1
kind: ConfigMap
metadata:
  name: {{ .name }}
  namespace: {{ .namespace }}
  labels:
    sycophant.md/command: "true"
    app.kubernetes.io/part-of: sycophant
{{- with .target }}
  annotations:
    sycophant.md/target: {{ . | toString | quote }}
{{- end }}
{{- with .data }}
data:
  {{- range $k, $v := . }}
  {{- if and (kindIs "float64" $v) (eq $v (float64 (int64 $v))) }}{{ $v = int64 $v }}{{ end }}
  {{ $k | quote }}: {{ $v | toString | quote }}
  {{- end }}
{{- end }}
{{- end -}}

{{- /* A reap command for kind .kind from its values block .v. */ -}}
{{- define "commands.reap" -}}
{{ include "commands.command" (dict "name" (include "commands.name" (dict "verb" "reap" "kind" .kind "target" .v.name)) "target" .v.name "namespace" (include "commands.tenantNs" .v.tenant)) }}
{{- end -}}

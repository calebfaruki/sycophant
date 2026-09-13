{{- define "sycophant.labels" -}}
app.kubernetes.io/managed-by: {{ .Release.Service }}
app.kubernetes.io/instance: {{ .Release.Name }}
app.kubernetes.io/part-of: sycophant
{{- end -}}

{{- define "sycophant.workspaceLabels" -}}
{{ include "sycophant.labels" .context }}
app.kubernetes.io/component: harness
app.kubernetes.io/name: {{ .name }}
{{- end -}}

{{- /*
One workspace's bindings entry: the toolsets it may call and, per grant, the
Secret name, mount path, and egress domain. Takes `.name` and `.workspace`.

A list item is either a bare toolset name or an object naming the toolset and
its grants. Both bind the same toolset by name; only the second exposes
credentials.

Rendered at zero indent; the caller nindents it under a `bindings.yaml` key.
With `.namesOnly` set it renders the names-only `relay-toolset-grants` the relay
serves on ListGrants -- the grant leaf collapses to `<grant>: {}`, no secret,
path, or egress.
*/}}
{{- define "sycophant.workspaceBindings" -}}
{{- $name := .name -}}
{{- $ws := .workspace -}}
{{- $namesOnly := .namesOnly -}}
{{ $name }}:
{{- if hasKey $ws "toolsets" }}
{{- range $toolset := $ws.toolsets }}
{{- if kindIs "string" $toolset }}
  - {{ $toolset }}
{{- else }}
  - name: {{ $toolset.name }}
    grants:
    {{- range $grant, $spec := $toolset.grants }}
    {{- if $namesOnly }}
      {{ $grant }}: {}
    {{- else }}
      {{ $grant }}:
        secret: {{ $spec.secret }}
        {{- if $spec.path }}
        path: {{ $spec.path }}
        {{- end }}
        {{- if $spec.egress }}
        egress: {{ $spec.egress }}
        {{- end }}
    {{- end }}
    {{- end }}
{{- end }}
{{- end }}
{{- else }}
  - stdlib
{{- end }}
{{- end -}}

{{- /*
Every model key is the DNS label the harness derives the inference Service
address from (`inference-<key>.<ns>.svc.cluster.local`), so it must be a valid
DNS label: lowercase alphanumerics and hyphens, no leading or trailing hyphen,
at most 63 characters. The chart render is the write path, so it rejects an
invalid key here. Requires the root context.
*/}}
{{- define "sycophant.validateModelKeys" -}}
{{- range $key, $_ := (default dict .Values.model) -}}
{{- if or (gt (len $key) 63) (not (regexMatch `^[a-z0-9]([-a-z0-9]*[a-z0-9])?$` $key)) -}}
{{- fail (printf "model key %q is not a valid DNS label. The harness derives the inference Service address from it (inference-<key>.<ns>.svc.cluster.local), so it must be lowercase alphanumerics and hyphens, no leading or trailing hyphen, at most 63 characters." $key) -}}
{{- end -}}
{{- end -}}
{{- end -}}

{{- /*
One workspace's capability manifest: every tool the workspace's bound toolsets
expose, each fully resolved with its argument schema and this workspace's grants.
Takes `.name`, `.workspace`, and the root `.context`.

The manifest is derived from the toolset IMAGES, not from Helm values: the
deploy driver runs `syco toolset manifest <image>` per bound image (merging this
workspace's grants) and hands the assembled document to the chart as the string
`workspaces.<ws>.capabilityManifest` (via `--set-file`). This helper emits that
string verbatim so both the ConfigMap and the harness pod-roll checksum see the
same bytes. A workspace with no supplied manifest renders an empty `{tools: []}`
document; the chart never sources tool schema from `toolsets.<name>.tools`.
*/}}
{{- define "sycophant.capabilityManifest" -}}
{{- $ws := .workspace -}}
{{- with (default "" $ws.capabilityManifest) -}}
{{- . -}}
{{- else -}}
{{- (dict "tools" (list)) | toYaml -}}
{{- end -}}
{{- end -}}

{{- /*
The per-workspace capability-manifest ConfigMap name, content-addressed: the
manifest hash is part of the name, so a changed manifest yields a NEW immutable
ConfigMap. The harness volume points at this same name, so a change swaps the
mounted object and rolls the pod, and helm prunes the superseded ConfigMap.
Deriving the name here keeps the ConfigMap and the volume from drifting. Takes
`.name`, `.workspace`, `.context`.
*/}}
{{- define "sycophant.capabilityManifestConfigMapName" -}}
capability-manifest-{{ .name }}-{{ include "sycophant.capabilityManifest" (dict "name" .name "workspace" .workspace "context" .context) | sha256sum | trunc 10 }}
{{- end -}}

{{- /*
Single derivation of a model's destination from its `baseUrl`.
Every consumer reads this; nothing else parses the URL. Takes `.model`, its
`.key`, and the root `.context`. Returns a JSON dict `{host, port, class}` for
`include ... | fromJson`, the idiom for a multi-valued helper.

Parse: split on `://` for scheme, take the authority up to the first `/`, split
on `:` for host and port. An absent port is 443 for https, 80 for http.

Classify, in the order the destination table fixes:
  endpoint  the key has an `inference` entry -> in-cluster Service
  cidr      an IPv4 literal host -> a /32
  fqdn      a dotted host -> an external name
  (fail)    anything else, including a bracketed IPv6 host the `:` split cannot
            read -- fail-closed, the table names <ip>/32 only.

The endpoint arm carries its own guard: the key and the host are authored
independently, so a model can name an inference entry yet point `baseUrl` at a
host that is not that entry's Service. Accept only the four resolvable forms of
`inference-<key>` and fail otherwise, so no selector is inferred from a host the
URL never named.
*/}}
{{- define "sycophant.modelDestination" -}}
{{- $key := .key -}}
{{- $ctx := .context -}}
{{- $baseUrl := .model.baseUrl -}}
{{- $scheme := first (splitList "://" $baseUrl) -}}
{{- $rest := last (splitList "://" $baseUrl) -}}
{{- $authority := first (splitList "/" $rest) -}}
{{- $hostPort := splitList ":" $authority -}}
{{- $host := first $hostPort -}}
{{- $port := "" -}}
{{- if gt (len $hostPort) 1 -}}
{{- $port = last $hostPort -}}
{{- else -}}
{{- $port = ternary "443" "80" (eq $scheme "https") -}}
{{- end -}}
{{- $inference := $ctx.Values.inference | default dict -}}
{{- $class := "" -}}
{{- if hasKey $inference $key -}}
{{- $ns := $ctx.Release.Namespace -}}
{{- $svc := printf "inference-%s" $key -}}
{{- $forms := list $svc (printf "%s.%s" $svc $ns) (printf "%s.%s.svc" $svc $ns) (printf "%s.%s.svc.cluster.local" $svc $ns) -}}
{{- if not (has $host $forms) -}}
{{- fail (printf "model %q has an inference entry but baseUrl host %q does not name its Service. Set the host to inference-%s (optionally suffixed .%s, .%s.svc, or .%s.svc.cluster.local), or remove the inference entry for %q." $key $host $key $ns $ns $ns $key) -}}
{{- end -}}
{{- $class = "endpoint" -}}
{{- else if regexMatch `^[0-9]{1,3}(\.[0-9]{1,3}){3}$` $host -}}
{{- $class = "cidr" -}}
{{- else if contains "." $host -}}
{{- $class = "fqdn" -}}
{{- else -}}
{{- fail (printf "model %q baseUrl host %q is not an inference Service, an IPv4 literal, or a dotted FQDN. Set baseUrl to an http/https URL whose host is one of those; bracketed IPv6 hosts are not supported." $key $host) -}}
{{- end -}}
{{- dict "host" $host "port" $port "class" $class | toJson -}}
{{- end -}}


{{- /*
Projected kube-apiserver SA token + CA + namespace at the canonical
kubelet mount path. Controllers use this in place of the auto-mounted
default token so the pod can carry `automountServiceAccountToken: false`
and still talk to the kube-apiserver via `kube::Client::try_default()`.
audience: omitted -- kubelet binds to apiserver default audience.
*/}}
{{- define "sycophant.projectedKubeApiToken.volume" -}}
- name: kube-api-token
  projected:
    defaultMode: 420
    sources:
      - serviceAccountToken:
          path: token
          expirationSeconds: 3600
      - configMap:
          name: kube-root-ca.crt
          items:
            - key: ca.crt
              path: ca.crt
      - downwardAPI:
          items:
            - path: namespace
              fieldRef:
                fieldPath: metadata.namespace
{{- end -}}

{{- define "sycophant.projectedKubeApiToken.mount" -}}
- name: kube-api-token
  mountPath: /var/run/secrets/kubernetes.io/serviceaccount
  readOnly: true
{{- end -}}

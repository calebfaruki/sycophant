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

Walks `workspaces.<ws>.toolsets[]` the same way `sycophant.workspaceBindings`
does. For each bound toolset it looks up the operator-authored tools under
`toolsets.<name>.tools` and emits each with `{name, description, parameters_json,
toolset, args, grants}`. It stores no Service address and no `models`/`inference`
section: the harness reads this file for tool schemas and secret bindings only,
and derives the inference address from the model catalog. Rendered as a YAML
document; the caller nindents it under a `manifest.yaml` key.
*/}}
{{- define "sycophant.capabilityManifest" -}}
{{- $ws := .workspace -}}
{{- $ctx := .context -}}
{{- $toolsets := $ctx.Values.toolsets | default dict -}}
{{- $tools := list -}}
{{- range $binding := (default list $ws.toolsets) -}}
{{- $tsname := "" -}}
{{- $grants := dict -}}
{{- if kindIs "string" $binding -}}
{{- $tsname = $binding -}}
{{- else -}}
{{- $tsname = $binding.name -}}
{{- $grants = (default dict $binding.grants) -}}
{{- end -}}
{{- $tsdef := (index $toolsets $tsname) | default dict -}}
{{- range $tool := (default list $tsdef.tools) -}}
{{- $args := list -}}
{{- range $arg := (default list $tool.args) -}}
{{- $args = append $args (dict "name" $arg.name "type" $arg.type "required" ($arg.required | default false) "env" $arg.env "description" ($arg.description | default "")) -}}
{{- end -}}
{{- $grantsOut := dict -}}
{{- range $g, $spec := $grants -}}
{{- $one := dict "secret" $spec.secret -}}
{{- with $spec.path }}{{- $_ := set $one "path" . }}{{- end -}}
{{- with $spec.egress }}{{- $_ := set $one "egress" . }}{{- end -}}
{{- $_ := set $grantsOut $g $one -}}
{{- end -}}
{{- $entry := dict "name" $tool.name "description" ($tool.description | default "") "parameters_json" ((default dict $tool.parameters) | toJson) "toolset" $tsname "args" $args "grants" $grantsOut -}}
{{- $tools = append $tools $entry -}}
{{- end -}}
{{- end -}}
{{- (dict "tools" $tools) | toYaml -}}
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

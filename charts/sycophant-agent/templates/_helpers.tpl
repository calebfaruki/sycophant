{{- define "sycophant.labels" -}}
app.kubernetes.io/managed-by: {{ .Release.Service }}
app.kubernetes.io/instance: {{ .Release.Name }}
app.kubernetes.io/part-of: sycophant
{{- end -}}

{{- define "sycophant.agentLabels" -}}
{{ include "sycophant.labels" .context }}
app.kubernetes.io/component: harness
app.kubernetes.io/name: {{ .name }}
{{- end -}}

{{- /*
An agent provision command's `secret.<toolset>.<secret-name>` keys, grouped as
YAML: toolset, then Secret name, then mount path. A Secret name may contain
dots, so the key splits into at most 3 parts. Takes the provision command data.
*/}}
{{- define "sycophant.commandSecrets" -}}
{{- $secrets := dict -}}
{{- range $k, $v := . -}}
{{- if hasPrefix "secret." $k -}}
{{- $parts := splitn "." 3 $k -}}
{{- if not (hasKey $secrets $parts._1) }}{{ $_ := set $secrets $parts._1 (dict) }}{{ end -}}
{{- $_ := set (get $secrets $parts._1) $parts._2 $v -}}
{{- end -}}
{{- end -}}
{{- toYaml $secrets -}}
{{- end -}}

{{- /*
An agent provision command's `egress.<toolset>` keys as YAML: toolset, then
its list of exact domains. Takes the provision command data.
*/}}
{{- define "sycophant.commandEgress" -}}
{{- $egress := dict -}}
{{- range $k, $v := . -}}
{{- if hasPrefix "egress." $k -}}
{{- $domains := compact (splitList "," $v) -}}
{{- if $domains }}{{ $_ := set $egress (trimPrefix "egress." $k) $domains }}{{ end -}}
{{- end -}}
{{- end -}}
{{- toYaml $egress -}}
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

{{- /*
The kube-dns egress every per-pod CNP opens: an L4 toEndpoints allowing UDP and
TCP :53 to the kube-system kube-dns pods. Identical across all CNPs; the L7
`rules.dns` allowlist a policy layers on top of it stays local. Caller nindents
to the egress-entry depth (4).
*/ -}}
{{- define "sycophant.dnsEgress" -}}
- toEndpoints:
    - matchLabels:
        io.kubernetes.pod.namespace: kube-system
        k8s-app: kube-dns
  toPorts:
    - ports:
        - port: "53"
          protocol: UDP
        - port: "53"
          protocol: TCP
{{- end -}}

{{- /*
The harness env an agent provision command sets, as a YAML map. The agent's
Deployment renders it as plain env. A sub-agent renders it into its
ag-<name>-harness ConfigMap, which its dispatch Jobs read. Takes the root
context.
*/ -}}
{{- define "sycophant.harnessEnv" -}}
{{- $name := .Values.name -}}
{{- $m := .Values.command -}}
{{- $secrets := dict -}}
{{- range $k, $v := $m }}{{ if hasPrefix "secret." $k }}{{ $_ := set $secrets $k $v }}{{ end }}{{ end -}}
# This harness serves only its own agent's instructions, read in-process from
# the read-only volume at INSTRUCTIONS_ROOT/AGENT_NAME (must match the
# instructions mountPath).
AGENT_NAME: {{ $name | quote }}
INSTRUCTIONS_ROOT: "/etc/instructions"
# The harness creates tool jobs via the kube API and dials each tool pod at its
# per-pod record under the capability Service. It serves no pod-facing
# surface, so a popped tool pod can open no socket back. The toolset map is
# watched in this namespace.
TOOLSET_NAMESPACE: {{ .Release.Namespace | quote }}
# The one model key this harness names. Every turn uses it. Its config is its
# own watched `inf-<key>` ConfigMap. An empty or unknown key fails every turn
# closed.
MODEL: {{ $m.model | default "" | quote }}
# How many archives a compact keeps (1 to 1000).
ARCHIVES: {{ $m.archives | default "10" | quote }}
# The toolsets this harness lists and the Secrets each of them gets. Each
# toolset's tools are its own watched `ts-<name>` ConfigMap. A toolset outside
# this list is never listed, and this harness sees no sibling's Secrets.
TOOLSETS: {{ $m.toolsets | default "" | quote }}
TOOLSET_SECRETS: {{ toJson $secrets | quote }}
TOOLSET_SCHEDULING_FILE: "/etc/sycophant/scheduling/scheduling.yaml"
# The per-agent headless Service that publishes each tool pod's per-pod record.
# Stamped as the pod subdomain and the middle DNS label the harness dials.
CAPABILITY_SERVICE_NAME: "ag-{{ $name }}-capability"
{{- with $m.parent }}
# The agent this one is a sub-agent of.
PARENT: {{ . | quote }}
{{- end }}
{{- end -}}

{{- /*
The instructions-sync env, as a YAML map. INSTRUCTIONS_SOURCE is the agent's
own folder in the store. Takes the root context.
*/ -}}
{{- define "sycophant.syncEnv" -}}
{{- $i := .Values.harness.instructions -}}
# readOnlyRootFilesystem: rclone's cache (under HOME) and its config path both
# point at the writable scratch emptyDir.
HOME: /tmp
RCLONE_CONFIG: /tmp/rclone.conf
# The `os` remote is defined by env alone, not by the config file: an S3
# backend against the SeaweedFS S3 gateway, provider Other, taking its
# credential from the AWS env vars below.
RCLONE_CONFIG_OS_TYPE: s3
RCLONE_CONFIG_OS_PROVIDER: Other
RCLONE_CONFIG_OS_ENV_AUTH: "true"
RCLONE_CONFIG_OS_ENDPOINT: "http://{{ $i.endpoint }}"
# The read grant is scoped to this folder, not the bucket, so skip rclone's
# bucket existence/create check: it would HeadBucket and 403.
RCLONE_CONFIG_OS_NO_CHECK_BUCKET: "true"
# Web identity: the pod's projected token is exchanged through the store's STS
# for a short-lived read-only credential. No static keys are set. They would
# take precedence over web identity.
AWS_WEB_IDENTITY_TOKEN_FILE: /var/run/secrets/objectstore/token
AWS_ROLE_ARN: "arn:aws:iam::role/agent-instructions-read"
AWS_ENDPOINT_URL_STS: "http://{{ $i.endpoint }}"
AWS_REGION: us-east-1
# The folder keyed by the pod's token subject. It ends with "/" so rclone lists
# the folder rather than HEADing a bare key the read grant denies.
INSTRUCTIONS_SOURCE: "os:{{ $i.bucket }}/system:serviceaccount:{{ .Release.Namespace }}:ag-{{ .Values.name }}/"
{{- end -}}

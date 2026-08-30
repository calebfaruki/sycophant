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
Single derivation of a prompt profile's turn destination from its `baseUrl`.
Every consumer reads this; nothing else parses the URL. Takes `.profile`, its
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
independently, so a profile can name an inference entry yet point `baseUrl` at a
host that is not that entry's Service. Accept only the four resolvable forms of
`inference-<key>` and fail otherwise, so no selector is inferred from a host the
URL never named.
*/}}
{{- define "sycophant.promptDestination" -}}
{{- $key := .key -}}
{{- $ctx := .context -}}
{{- $baseUrl := .profile.baseUrl -}}
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
{{- fail (printf "prompt profile %q has an inference entry but baseUrl host %q does not name its Service. Set the host to inference-%s (optionally suffixed .%s, .%s.svc, or .%s.svc.cluster.local), or remove the inference entry for %q." $key $host $key $ns $ns $ns $key) -}}
{{- end -}}
{{- $class = "endpoint" -}}
{{- else if regexMatch `^[0-9]{1,3}(\.[0-9]{1,3}){3}$` $host -}}
{{- $class = "cidr" -}}
{{- else if contains "." $host -}}
{{- $class = "fqdn" -}}
{{- else -}}
{{- fail (printf "prompt profile %q baseUrl host %q is not an inference Service, an IPv4 literal, or a dotted FQDN. Set baseUrl to an http/https URL whose host is one of those; bracketed IPv6 hosts are not supported." $key $host) -}}
{{- end -}}
{{- dict "host" $host "port" $port "class" $class | toJson -}}
{{- end -}}

{{- /*
The universal egress minimum every tool-job pod needs: kube-dns:53 with an L7
DNS allowlist pinned to the toolset-ctrl FQDN, plus toolset-ctrl:9090 for tool
dispatch. A policy that ADDS a domain must carry its own `rules.dns` on :53
alongside this floor (the L4-shadows-L7 hazard documented in
harness-netpol.yaml). Rendered as a list of egress rules; the caller nindents
it under `egress:`. Requires the root context.
*/}}
{{- define "sycophant.toolJobDnsFloor" -}}
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
      rules:
        dns:
          - matchName: "toolset-ctrl.{{ .Release.Namespace }}.svc.cluster.local"
- toEndpoints:
    - matchLabels:
        app.kubernetes.io/component: toolset-ctrl
  toPorts:
    - ports:
        - port: "9090"
          protocol: TCP
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

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
An agent provision command's `grant.<toolset>.<grant>.<secret|path|egress>`
keys, grouped as YAML: toolset, then grant, then field. Takes the provision
command data.
*/}}
{{- define "sycophant.commandGrants" -}}
{{- $grants := dict -}}
{{- range $k, $v := . -}}
{{- if hasPrefix "grant." $k -}}
{{- $parts := splitList "." $k -}}
{{- $toolset := index $parts 1 -}}
{{- $grant := index $parts 2 -}}
{{- if not (hasKey $grants $toolset) }}{{ $_ := set $grants $toolset (dict) }}{{ end -}}
{{- $byGrant := get $grants $toolset -}}
{{- if not (hasKey $byGrant $grant) }}{{ $_ := set $byGrant $grant (dict) }}{{ end -}}
{{- $_ := set (get $byGrant $grant) (index $parts 3) $v -}}
{{- end -}}
{{- end -}}
{{- toYaml $grants -}}
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

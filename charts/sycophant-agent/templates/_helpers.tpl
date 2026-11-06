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

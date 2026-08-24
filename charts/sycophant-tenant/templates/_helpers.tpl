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
The universal egress minimum every tool-job pod needs: kube-dns:53 with an L7
DNS allowlist pinned to the toolset-ctrl FQDN, plus toolset-ctrl:9090 for tool
dispatch. Cilium unions same-PortProtocol L7 DNS rules across policies, so a
policy that ADDS a domain must carry its own `rules.dns` on :53 alongside this
floor or it shadows the pinned allowlist. Rendered as a list of egress rules;
the caller nindents it under `egress:`. Requires the root context.
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

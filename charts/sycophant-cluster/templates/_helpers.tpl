{{/*
Guard for the Kyverno policies. The cluster chart always renders them; this
helper only fails loudly when the Kyverno CRDs are absent, so a Kyverno-less
cluster errors at install instead of silently shipping without the
tenant-isolation policies. `fail` fires on `include`, so the guard cannot be
bypassed. Installing the Kyverno controller is a cluster prerequisite the
operator or the quickstart bundle handles, not a chart values knob.

The probe is ValidatingPolicy because the CEL types arrived after the legacy
ones: a cluster whose CRDs predate it cannot serve any policy this chart ships.
*/}}
{{- define "sycophant.renderKyverno" -}}
{{- if not (.Capabilities.APIVersions.Has "policies.kyverno.io/v1/ValidatingPolicy") -}}
{{- fail "the Kyverno CRDs are absent (policies.kyverno.io/v1/ValidatingPolicy not found). Install the kyverno-crds chart (or run `syco setup`, which installs Kyverno) before installing sycophant-cluster." -}}
{{- end -}}
true
{{- end -}}

{{/*
Command contract: a provision, reap or suspend command is a ConfigMap carrying
the label below, whose name selects the reactor. The tenant prefix
keeps every provisioned namespace out of the reserved ones.
*/}}
{{- define "sycophant.commandLabel" -}}sycophant.md/command{{- end -}}
{{- define "sycophant.provisionCommandName" -}}provision-tenant{{- end -}}
{{- define "sycophant.workspaceProvisionCommandName" -}}provision-workspace{{- end -}}
{{- define "sycophant.userProvisionCommandName" -}}provision-user{{- end -}}
{{- define "sycophant.toolsetProvisionCommandName" -}}provision-toolset{{- end -}}
{{- define "sycophant.inferenceProvisionCommandName" -}}provision-inference{{- end -}}
{{- define "sycophant.channelProvisionCommandName" -}}provision-channel{{- end -}}
{{- define "sycophant.instructionsProvisionCommandName" -}}provision-instructions{{- end -}}
{{- define "sycophant.suspendCommandName" -}}suspend-tenant{{- end -}}
{{- define "sycophant.reapAfterAnnotation" -}}sycophant.md/reap-after{{- end -}}
{{- define "sycophant.tenantNsPrefix" -}}tenant-{{- end -}}

{{/*
The capability job gate's creating identity, shared by its mutate and its
validate so the two cannot drift onto different notions of "the workspace".

`parseServiceAccount` pulls NS/NAME out of `system:serviceaccount:NS:NAME`.
The match conditions are the gate: the creator must be a ServiceAccount in the
namespace it is writing to, and its name must be `ws-<workspace>`. A Job
from any other identity is not this gate's business and falls through to the
rest of admission.
*/}}
{{- define "sycophant.capabilityJobGateIdentity" -}}
matchConditions:
  - name: creator-is-a-harness-in-this-namespace
    expression: >-
      request.userInfo.username.startsWith("system:serviceaccount:")
        && parseServiceAccount(request.userInfo.username).Namespace == request.namespace
        && parseServiceAccount(request.userInfo.username).Name.startsWith("ws-")
variables:
  - name: workspace
    expression: >-
      parseServiceAccount(request.userInfo.username).Name.substring(3)
  - name: pod
    expression: "object.spec.template.spec"
  - name: containers
    expression: "variables.pod.containers"
  - name: initContainers
    expression: "variables.pod.?initContainers.orValue([])"
  - name: allContainers
    expression: "variables.containers + variables.initContainers"
{{- end -}}

{{/* Match for the reactors on the tenant provision command in sycophant-system. */}}
{{- define "sycophant.provisionCommandSystemMatchConstraints" -}}
matchConstraints:
  namespaceSelector:
    matchLabels:
      kubernetes.io/metadata.name: sycophant-system
  objectSelector:
    matchLabels:
      {{ include "sycophant.commandLabel" . }}: "true"
  resourceRules:
    - apiGroups: [""]
      apiVersions: ["v1"]
      operations: ["CREATE", "UPDATE"]
      resources: ["configmaps"]
matchConditions:
  - name: command-name-is-a-tenant-command
    expression: >-
      object.metadata.name.startsWith('{{ include "sycophant.provisionCommandName" . }}-')
{{- end -}}

{{- define "sycophant.labels" -}}
{{ include "sycophant.baseLabels" . }}
app.kubernetes.io/part-of: sycophant
{{- end -}}

{{/*
`sycophant.labels` without `part-of`, for the one resource that carries a
different one: the generated tenant Namespace is `part-of: sycophant-tenant`,
which is the label every tenant-scoped policy triggers on. Emitting both would
be a duplicate mapping key, which a generate template is parsed strictly enough
to reject.
*/}}
{{- define "sycophant.baseLabels" -}}
app.kubernetes.io/managed-by: {{ .Release.Service }}
app.kubernetes.io/instance: {{ .Release.Name }}
{{- end -}}

{{/* The IdP user prefix the apiserver stamps on an OIDC identity, `<issuer>#`. */}}
{{- define "sycophant.idpUserPrefix" -}}{{ .Values.idp.issuer }}#{{- end -}}

{{/*
Non-empty when the self-service setting is on. Fails the render when the
operator identity is absent or lacks the issuer prefix.
*/}}
{{- define "sycophant.selfService" -}}
{{- if .Values.selfService.enabled -}}
{{- $operator := .Values.selfService.operator | default "" -}}
{{- if not (hasPrefix (include "sycophant.idpUserPrefix" .) $operator) -}}
{{- fail (printf "selfService.operator must be set and start with the IdP issuer prefix %s" (include "sycophant.idpUserPrefix" .)) -}}
{{- end -}}
true
{{- end -}}
{{- end -}}

{{/* The group the apiserver puts every configured-issuer login in. */}}
{{- define "sycophant.tenantAdminGroup" -}}sycophant:tenant-admins{{- end -}}

{{- /*
One single-shot install per provision command. The stack policy stamps a
ServiceAccount and a Job in sycophant-system. The Job runs its kind's script
with the ServiceAccount's projected token. The ServiceAccount lives outside the
tenant namespace, so it stays an external caller to the tenant-scoped write
protections and needs no exemption from them.

The grant policy binds that ServiceAccount to its kind's ClusterRole in the
tenant namespace, so RBAC bounds its reach to that one namespace. It is a
separate policy because its target namespace may not exist yet (the tenant
provision command's sibling reactor creates it), and its retry must never
re-apply the Job, whose spec.selector is immutable.

Every generated object is named `provision-<kind>-<uid10>`, from the provision
command's uid, so a re-created command never collides with an earlier run's
Job, even within its TTL.

The tenant provision command lives in sycophant-system, every other in the
tenant namespace it targets. Only
the tenant chart can render the Namespace, and its install forces
namespace.create=false so a provision command cannot manage it. Only the tenant
install takes ownership of existing objects.

Provision commands are one-shot: once its script succeeds the Job deletes its
command. A failed install exits first and leaves the command in place.
*/ -}}
{{- define "sycophant.provisionInstallerMatch" -}}
{{- $ctx := .ctx -}}
{{- if eq .kind "tenant" -}}
{{ include "sycophant.provisionCommandSystemMatchConstraints" $ctx }}
{{- else -}}
{{- $prefix := .prefix | default (include (printf "sycophant.%sProvisionCommandName" .kind) $ctx) -}}
matchConstraints:
  namespaceSelector:
    matchLabels:
      app.kubernetes.io/part-of: sycophant-tenant
  objectSelector:
    matchLabels:
      {{ include "sycophant.commandLabel" $ctx }}: "true"
  resourceRules:
    - apiGroups: [""]
      apiVersions: ["v1"]
      operations: {{ .operations | default (list "CREATE" "UPDATE") | toJson }}
      resources: ["configmaps"]
# resourceNames cannot hold a prefix, so the apiserver drops a foreign provision
# command here before Kyverno is called.
matchConditions:
  - name: command-name-is-a-{{ .prefix | default .kind }}-command
    expression: >-
      object.metadata.name.startsWith('{{ $prefix }}-')
{{- end }}
variables:
  - name: tenant
{{- if eq .kind "tenant" }}
    expression: '"{{ include "sycophant.tenantNsPrefix" $ctx }}" + object.data["tenant"]'
{{- else }}
    expression: 'object.metadata.namespace'
{{- end }}
  - name: uid10
    expression: 'object.metadata.uid.replace("-", "").substring(0, 10)'
  # Past the Job's activeDeadline, so the reap removes the identity only after
  # the install has finished or timed out.
  - name: reapAfter
    expression: 'string(time.now() + duration("1800s"))'
{{- end -}}

{{- /*
The ServiceAccount and Job a lifecycle reactor stamps in sycophant-system, both
named `<name>-<uid10>` and labelled component `<name>`. Params: ctx, name, script. Rendered at zero
indent inside a CEL-interpolated generate template.
*/ -}}
{{- define "sycophant.provisionerJob" -}}
{{- $ctx := .ctx -}}
apiVersion: v1
kind: ServiceAccount
metadata:
  name: (( "{{ .name }}-" + variables.uid10 ))
  namespace: sycophant-system
  labels:
    {{- include "sycophant.labels" $ctx | nindent 4 }}
    app.kubernetes.io/component: {{ .name }}
  annotations:
    {{ include "sycophant.reapAfterAnnotation" $ctx }}: (( variables.reapAfter ))
---
apiVersion: batch/v1
kind: Job
metadata:
  name: (( "{{ .name }}-" + variables.uid10 ))
  namespace: sycophant-system
  labels:
    {{- include "sycophant.labels" $ctx | nindent 4 }}
    app.kubernetes.io/component: {{ .name }}
spec:
  backoffLimit: 4
  activeDeadlineSeconds: 900
  ttlSecondsAfterFinished: 600
  template:
    metadata:
      labels:
        {{- include "sycophant.labels" $ctx | nindent 8 }}
        app.kubernetes.io/component: {{ .name }}
    spec:
      restartPolicy: OnFailure
      serviceAccountName: (( "{{ .name }}-" + variables.uid10 ))
      automountServiceAccountToken: false
      securityContext:
        runAsNonRoot: true
        runAsUser: 65532
        runAsGroup: 65532
        seccompProfile:
          type: RuntimeDefault
      containers:
        - name: helm
          image: "{{ $ctx.Values.provisioner.image }}:{{ $ctx.Values.provisioner.tag }}"
          imagePullPolicy: {{ $ctx.Values.provisioner.pullPolicy }}
          securityContext:
            readOnlyRootFilesystem: true
            allowPrivilegeEscalation: false
            capabilities:
              drop: [ALL]
          resources:
            requests: { cpu: "50m", memory: "128Mi" }
            limits:   { cpu: "500m", memory: "256Mi" }
          env:
            - name: HOME
              value: /tmp
            # Release state lives in ConfigMaps, which the
            # provision-write role can read; Secrets are write-only.
            - name: HELM_DRIVER
              value: configmap
            - name: TENANT
              value: (( variables.tenant ))
            - name: COMMAND
              value: (( object.metadata.name ))
            - name: COMMAND_UID
              value: (( object.metadata.uid ))
            {{- if eq .name "provision-tenant" }}
            - name: HS_ENABLED
              value: '(( "headscale.enabled" in object.data ? object.data["headscale.enabled"] : "" ))'
            - name: HS_SERVER_URL
              value: '(( "headscale.serverUrl" in object.data ? object.data["headscale.serverUrl"] : "" ))'
            - name: HS_MAGIC_DNS
              value: '(( "headscale.magicDns" in object.data ? object.data["headscale.magicDns"] : "" ))'
            - name: HS_BASE_DOMAIN
              value: '(( "headscale.baseDomain" in object.data ? object.data["headscale.baseDomain"] : "" ))'
            - name: HS_STORAGE_CLASS
              value: '(( "headscale.storageClass" in object.data ? object.data["headscale.storageClass"] : "" ))'
            - name: HS_ACME_ENABLED
              value: '(( "headscale.acme.enabled" in object.data ? object.data["headscale.acme.enabled"] : "" ))'
            - name: HS_ACME_EMAIL
              value: '(( "headscale.acme.email" in object.data ? object.data["headscale.acme.email"] : "" ))'
            {{- end }}
          volumeMounts:
            - name: tmp
              mountPath: /tmp
            - name: kube-api-token
              mountPath: /var/run/secrets/kubernetes.io/serviceaccount
              readOnly: true
          command: [sh, -c]
          args:
            - |
              {{- .script | trim | nindent 14 }}
      volumes:
        - name: tmp
          emptyDir: {}
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

{{- /* The release name prefix a capability install uses for its kind. */ -}}
{{- define "sycophant.releasePrefix" -}}
{{- get (dict "workspace" "ws" "toolset" "ts" "inference" "inf" "user" "user" "channel" "ch") . -}}
{{- end -}}

{{- /* Read the command into CM; stop if it is gone or was replaced. */ -}}
{{- define "sycophant.commandFetch" -}}
CM="$(kubectl get configmap "$COMMAND" -n "$TENANT" -o json)" \
  || { echo "command $COMMAND is gone; this attempt is stale" >&2; exit 1; }
CM_UID="$(printf '%s' "$CM" | jq -r '.metadata.uid')"
[ "$CM_UID" = "$COMMAND_UID" ] || { echo "command $COMMAND was replaced (uid $CM_UID != $COMMAND_UID); this attempt is stale" >&2; exit 1; }
{{- end -}}

{{- /* The install script for one kind; see sycophant.provisionInstaller. */ -}}
{{- define "sycophant.provisionInstallScript" -}}
{{- $ctx := .ctx -}}
{{- $kind := .kind -}}
set -eu
{{- if eq $kind "tenant" }}
# The sibling reactor creates the Namespace from the same provision command.
for i in $(seq 1 60); do
  kubectl get namespace "$TENANT" >/dev/null 2>&1 && break
  sleep 2
done
{{- if $ctx.Values.idp.certificateAuthority }}
cat > /tmp/idp-ca.pem <<'IDPCA'
{{ $ctx.Values.idp.certificateAuthority }}
IDPCA
{{- end }}
# Build headscale values file from CEL-injected env vars. jq --arg escapes
# all values; empty keys are omitted so the tenant chart's defaults hold.
# Booleans are converted from the string "true"/"false" to JSON native booleans.
jq -n \
  --arg enabled    "$HS_ENABLED" \
  --arg serverUrl  "$HS_SERVER_URL" \
  --arg magicDns   "$HS_MAGIC_DNS" \
  --arg baseDomain "$HS_BASE_DOMAIN" \
  --arg storageClass "$HS_STORAGE_CLASS" \
  --arg acmeEnabled "$HS_ACME_ENABLED" \
  --arg acmeEmail   "$HS_ACME_EMAIL" \
  '{headscale: (
    (if $enabled    != "" then {enabled:   ($enabled   == "true")} else {} end) +
    (if $serverUrl  != "" then {serverUrl: $serverUrl}             else {} end) +
    (if $magicDns   != "" then {magicDns:  ($magicDns  == "true")} else {} end) +
    (if $baseDomain != "" then {baseDomain: $baseDomain}           else {} end) +
    (if $storageClass != "" then {storageClass: $storageClass}     else {} end) +
    (if $acmeEnabled != "" or $acmeEmail != "" then
      {acme: (
        (if $acmeEnabled != "" then {enabled: ($acmeEnabled == "true")} else {} end) +
        (if $acmeEmail   != "" then {email: $acmeEmail}                else {} end)
      )}
    else {} end)
  )}' > /tmp/hs-values.yaml
helm upgrade --install "$TENANT" {{ $ctx.Values.provisioner.chartsDir }}/sycophant-tenant \
  --namespace "$TENANT" \
  --reset-then-reuse-values \
  --set namespace.create=false \
  --set-string idp.issuer={{ $ctx.Values.idp.issuer }} \
  --set-string idp.audience={{ $ctx.Values.idp.audience }} \
{{- if $ctx.Values.idp.certificateAuthority }}
  --set-file idp.certificateAuthority=/tmp/idp-ca.pem \
{{- end }}
  -f /tmp/hs-values.yaml \
  --take-ownership \
  --timeout 10m
kubectl delete configmap "$COMMAND" -n sycophant-system
{{- else }}
NAME="${COMMAND#provision-{{ $kind }}-}"
# The provision command's flat data rides into the chart verbatim as
# .Values.command. Read by plain assignment so a missing command (deleted
# under a retry) stops the install: helm given empty values would prune the
# release.
# Read the command in one get: uid, data and (for user) the write time.
{{ include "sycophant.commandFetch" . }}
DATA="$(printf '%s' "$CM" | jq -c '.data // {}')"
printf '{"command":%s}' "$DATA" > /tmp/values.yaml
{{- if eq $kind "user" }}
CREATED="$(printf '%s' "$CM" | jq -r '.metadata.creationTimestamp')"
{{- end }}
helm upgrade --install "{{ include "sycophant.releasePrefix" $kind }}-$NAME" {{ $ctx.Values.provisioner.chartsDir }}/sycophant-{{ $kind }} \
  --namespace "$TENANT" \
  --set-string name="$NAME" \
{{- if eq $kind "user" }}
  --set-string commandCreated="$CREATED" \
{{- end }}
  -f /tmp/values.yaml \
  --timeout 10m
kubectl delete configmap "$COMMAND" -n "$TENANT"
{{- end }}
{{- end -}}

{{- define "sycophant.provisionInstaller" -}}
{{- $ctx := .ctx -}}
{{- $kind := .kind -}}
{{- $component := printf "provision-%s" $kind -}}
{{- $script := .script | default (include "sycophant.provisionInstallScript" (dict "ctx" $ctx "kind" $kind)) -}}
{{- $role := .role | default "provision-write" -}}
apiVersion: policies.kyverno.io/v1
kind: GeneratingPolicy
metadata:
  name: {{ ternary "provision-tenant-stack" $component (eq $kind "tenant") }}
  labels:
    {{- include "sycophant.labels" $ctx | nindent 4 }}
spec:
  evaluation:
    generateExisting:
      enabled: false
    synchronize:
      enabled: false
  {{- include "sycophant.provisionInstallerMatch" (dict "ctx" $ctx "kind" $kind) | nindent 2 }}
  generate:
    - template:
        interpolate: cel
        value: |
          {{- include "sycophant.provisionerJob" (dict "ctx" $ctx "name" $component "script" $script) | nindent 10 }}
---
apiVersion: policies.kyverno.io/v1
kind: GeneratingPolicy
metadata:
  name: {{ $component }}-grant
  labels:
    {{- include "sycophant.labels" $ctx | nindent 4 }}
spec:
  evaluation:
    generateExisting:
      enabled: false
    synchronize:
      enabled: false
  {{- include "sycophant.provisionInstallerMatch" (dict "ctx" $ctx "kind" $kind) | nindent 2 }}
  generate:
    - template:
        interpolate: cel
        value: |
          {{- include "sycophant.provisionerBinding" (dict "ctx" $ctx "name" $component "namespace" "(( variables.tenant ))" "kind" "ClusterRole" "role" $role) | nindent 10 }}
          {{- if eq $kind "tenant" }}
          ---
          # Lets the install delete its own command once helm succeeds, and no
          # other tenant's.
          apiVersion: rbac.authorization.k8s.io/v1
          kind: Role
          metadata:
            name: (( "{{ $component }}-" + variables.uid10 ))
            namespace: sycophant-system
            labels:
              {{- include "sycophant.labels" $ctx | nindent 14 }}
              app.kubernetes.io/component: {{ $component }}
            annotations:
              {{ include "sycophant.reapAfterAnnotation" $ctx }}: (( variables.reapAfter ))
          rules:
            - apiGroups: [""]
              resources: ["configmaps"]
              resourceNames:
                - (( object.metadata.name ))
              verbs: ["delete"]
          ---
          {{- include "sycophant.provisionerBinding" (dict "ctx" $ctx "name" $component "namespace" "sycophant-system" "kind" "Role" "role" "(( \"provision-tenant-\" + variables.uid10 ))") | nindent 10 }}
          {{- end }}
{{- end -}}

{{- /* A RoleBinding named `<name>-<uid10>`, labelled component `<name>`, for the reactor's ServiceAccount. */ -}}
{{- define "sycophant.provisionerBinding" -}}
apiVersion: rbac.authorization.k8s.io/v1
kind: RoleBinding
metadata:
  name: (( "{{ .name }}-" + variables.uid10 ))
  namespace: {{ .namespace }}
  labels:
    {{- include "sycophant.labels" .ctx | nindent 4 }}
    app.kubernetes.io/component: {{ .name }}
  annotations:
    {{ include "sycophant.reapAfterAnnotation" .ctx }}: (( variables.reapAfter ))
subjects:
  - kind: ServiceAccount
    name: (( "{{ .name }}-" + variables.uid10 ))
    namespace: sycophant-system
roleRef:
  apiGroup: rbac.authorization.k8s.io
  kind: {{ .kind }}
  name: {{ .role }}
{{- end -}}

{{- /* The uninstall script for one kind; see sycophant.provisionUninstaller. */ -}}
{{- define "sycophant.provisionUninstallScript" -}}
set -eu
NAME="${COMMAND#reap-{{ .kind }}-}"
helm uninstall "{{ include "sycophant.releasePrefix" .kind }}-$NAME" -n "$TENANT" --timeout 10m
kubectl delete configmap "$COMMAND" -n "$TENANT"
{{- end -}}

{{- /*
One single-shot uninstall per reap command `reap-<kind>-<name>` in a tenant
namespace. The ServiceAccount and Job `uninstall-<kind>-<uid10>` live in
sycophant-system. The RoleBinding of the same name binds them to the kind's
ClusterRole in the reap command's namespace. The Job runs its kind's script,
then deletes the reap command. A failed run exits first and leaves the command
in place. The ServiceAccount and RoleBinding carry component `uninstall-<kind>`
and the reap-after annotation, so `reap-<kind>` sweeps them.
*/ -}}
{{- define "sycophant.provisionUninstaller" -}}
{{- $ctx := .ctx -}}
{{- $kind := .kind -}}
{{- $name := printf "uninstall-%s" $kind -}}
{{- $script := .script | default (include "sycophant.provisionUninstallScript" (dict "kind" $kind)) -}}
{{- $role := .role | default "provision-write" -}}
apiVersion: policies.kyverno.io/v1
kind: GeneratingPolicy
metadata:
  name: {{ $name }}
  labels:
    {{- include "sycophant.labels" $ctx | nindent 4 }}
spec:
  evaluation:
    generateExisting:
      enabled: false
    synchronize:
      enabled: false
  {{- include "sycophant.provisionInstallerMatch" (dict "ctx" $ctx "kind" $kind "prefix" (printf "reap-%s" $kind) "operations" (list "CREATE")) | nindent 2 }}
  generate:
    - template:
        interpolate: cel
        value: |
          {{- include "sycophant.provisionerJob" (dict "ctx" $ctx "name" $name "script" $script) | nindent 10 }}
          ---
          {{- include "sycophant.provisionerBinding" (dict "ctx" $ctx "name" $name "namespace" "(( variables.tenant ))" "kind" "ClusterRole" "role" $role) | nindent 10 }}
{{- end -}}

{{/*
Reaps each install and uninstall ServiceAccount and RoleBinding once its
reap-after instant passes, so no apply-capable credential persists between runs.
Each Job self-cleans on its own ttlSecondsAfterFinished.
*/}}
{{- define "sycophant.provisionReap" -}}
{{- $ctx := .ctx -}}
{{- $kind := .kind -}}
apiVersion: policies.kyverno.io/v1
kind: DeletingPolicy
metadata:
  name: reap-{{ $kind }}
  labels:
    {{- include "sycophant.labels" $ctx | nindent 4 }}
spec:
  schedule: {{ $ctx.Values.reap.schedule | quote }}
  matchConstraints:
    objectSelector:
      matchExpressions:
        - key: app.kubernetes.io/component
          operator: In
          values: [provision-{{ $kind }}, uninstall-{{ $kind }}]
    resourceRules:
      - apiGroups: [""]
        apiVersions: ["v1"]
        operations: ["CREATE", "UPDATE"]
        resources: ["serviceaccounts"]
      - apiGroups: ["rbac.authorization.k8s.io"]
        apiVersions: ["v1"]
        operations: ["CREATE", "UPDATE"]
        resources: ["rolebindings"]
  variables:
    - name: reapAfter
      expression: >-
        object.metadata.?annotations["{{ include "sycophant.reapAfterAnnotation" $ctx }}"].orValue("")
  conditions:
    - name: reap-after-ttl-has-passed
      expression: >-
        variables.reapAfter != "" && timestamp(variables.reapAfter) < time.now()
{{- end -}}

{{/*
The service name — the chart name (baked from tonin.toml [service].name at
generate time), NOT the Helm release name. Used for resource names, the binary
path (/usr/local/bin/<name>), and the `service.identity: <name>.<ns>` mesh label
that callers' NetworkPolicies match — all fixed properties of the service,
independent of how the release happens to be named.
*/}}
{{- define "service.name" -}}
{{- .Chart.Name }}
{{- end }}

{{/*
Common labels applied to every resource.
*/}}
{{- define "service.labels" -}}
app: {{ include "service.name" . }}
app.kubernetes.io/name: {{ include "service.name" . }}
app.kubernetes.io/version: {{ .Values.image.tag | quote }}
helm.sh/chart: {{ .Chart.Name }}-{{ .Chart.Version }}
{{- end }}

{{/*
Selector labels (stable subset — must not change after first deploy).
*/}}
{{- define "service.selectorLabels" -}}
app: {{ include "service.name" . }}
{{- end }}

{{/*
The service's container image: `repository@digest` when image.digest is set
(an immutable, digest-pinned image, e.g. from a `tonin deploy` lock), else
`repository:tag`. Every container that runs the service image uses this.
*/}}
{{- define "service.image" -}}
{{- if .Values.image.digest -}}
{{- printf "%s@%s" .Values.image.repository .Values.image.digest -}}
{{- else -}}
{{- printf "%s:%s" .Values.image.repository (toString .Values.image.tag) -}}
{{- end -}}
{{- end }}

{{/*
Databases and caches are references to the platform contract (tonin.toml
[database]/[cache], resolved into the `datastores` values list): a Secret key
(or a literal dev `url`) read into an env var, an optional Postgres CA mount
for sslmode=verify-full, and an egress rule to the datastore's pods.
*/}}

{{/*
Container env of every datastore. A Secret-sourced var always comes before the
var that composes from it with $(VAR) (verify-full), so Kubernetes expands it.
*/}}
{{- define "service.datastoreEnv" -}}
{{- range .Values.datastores }}
{{- with .secret }}
- name: {{ .env }}
  valueFrom:
    secretKeyRef:
      name: {{ .name }}
      key: {{ .key }}
{{- end }}
{{- if .value }}
- name: {{ .env }}
  value: {{ .value | quote }}
{{- end }}
{{- end }}
{{- end }}

{{/*
Pod volumes: only the CA key (`ca.crt`) of each verify-full database's CA Secret.
*/}}
{{- define "service.datastoreVolumes" -}}
{{- range .Values.datastores }}
{{- with .caMount }}
- name: {{ .volume }}
  secret:
    secretName: {{ .secret }}
    items:
      - key: {{ .key }}
        path: {{ .key }}
{{- end }}
{{- end }}
{{- end }}

{{/*
Container volumeMounts for the volumes above.
*/}}
{{- define "service.datastoreVolumeMounts" -}}
{{- range .Values.datastores }}
{{- with .caMount }}
- name: {{ .volume }}
  mountPath: {{ .mountPath }}
  readOnly: true
{{- end }}
{{- end }}
{{- end }}

{{/*
CiliumNetworkPolicy egress rules to each datastore's pods (contract labels, in
the service's namespace unless `namespace` is set).
*/}}
{{- define "service.datastoreEgress" -}}
{{- range .Values.datastores }}
{{- $name := .name }}
{{- with .egress }}
# [{{ $name }}] (platform contract)
- toEndpoints:
    - matchLabels:
        {{- if .namespace }}
        k8s:io.kubernetes.pod.namespace: {{ .namespace }}
        {{- end }}
        {{- range $k, $v := .labels }}
        {{ $k }}: {{ $v | quote }}
        {{- end }}
  toPorts:
    - ports:
        - { port: "{{ .port }}", protocol: TCP }
{{- end }}
{{- end }}
{{- end }}

{{/*
Container volumeMounts: the datastore CA mounts, then every [[volumes]] entry
(sorted by mount path, so a parent is mounted before a volume nested in it).
Call with (dict "root" $ "pvc" true|false): pvc=false leaves out the `pvc`
volumes (for pods outside the StatefulSet, e.g. the migration Job).
*/}}
{{- define "service.volumeMounts" -}}
{{- include "service.datastoreVolumeMounts" .root }}
{{- $pvc := .pvc }}
{{- range .root.Values.volumes }}
{{- if or $pvc (ne .type "pvc") }}
- name: {{ .name }}
  mountPath: {{ .mountPath }}
  {{- if .readOnly }}
  readOnly: true
  {{- end }}
{{- end }}
{{- end }}
{{- end }}

{{/*
Pod volumes: the datastore CA Secrets and the `empty-dir` [[volumes]]
(`pvc` volumes come from the StatefulSet's volumeClaimTemplates).
*/}}
{{- define "service.podVolumes" -}}
{{- include "service.datastoreVolumes" . }}
{{- range .Values.volumes }}
{{- if eq .type "empty-dir" }}
- name: {{ .name }}
  {{- if .emptyDir }}
  emptyDir:
    {{- toYaml .emptyDir | nindent 4 }}
  {{- else }}
  emptyDir: {}
  {{- end }}
{{- end }}
{{- end }}
{{- end }}

{{/*
An httpGet or grpc probe action. Call with (dict "root" $ "path" <path>).
*/}}
{{- define "service.probeAction" -}}
{{- $h := .root.Values.service.health -}}
{{- if $h.grpc -}}
grpc:
  port: {{ $h.port }}
{{- else -}}
httpGet:
  path: {{ .path | quote }}
  port: {{ $h.port }}
{{- end -}}
{{- end }}

{{/*
The minimum number of replicas the service runs: autoscaling.minReplicas with
the HPA on, else replicaCount. The PDB and topology spread need more than one.
*/}}
{{- define "service.minReplicas" -}}
{{- if .Values.autoscaling.enabled -}}
{{- .Values.autoscaling.minReplicas -}}
{{- else -}}
{{- .Values.replicaCount -}}
{{- end -}}
{{- end }}

{{/*
Pod-spec fields shared by every pod tonin renders: ServiceAccount, token
automount, imagePullSecrets, pod securityContext, nodeSelector, tolerations
and (service pods only) topologySpreadConstraints. Include it once per pod
spec, at the pod spec's indentation:

  spec:
    {{- include "service.podSpecCommon" (dict "root" . "component" "server") | nindent 6 }}

`root` is the chart's top-level context (`$` inside a range). `component`:
  server  — the service's own pods (Deployment, StatefulSet): the chart
            ServiceAccount, plus topology spread when minReplicas > 1;
  migrate — the migration hook Job: the hook-scoped `<service>-migrate`
            ServiceAccount (the chart's own does not exist yet on a first
            install), never an API token;
  cronjob — CronJob pods: the chart ServiceAccount, no spread.
*/}}
{{- define "service.podSpecCommon" -}}
{{- $root := .root -}}
{{- $component := .component | default "server" -}}
{{- $serviceAccount := include "service.name" $root -}}
{{- if eq $component "migrate" -}}
{{- $serviceAccount = printf "%s-migrate" $serviceAccount -}}
{{- end -}}
serviceAccountName: {{ $serviceAccount }}
automountServiceAccountToken: {{ and (ne $component "migrate") $root.Values.serviceAccount.automountToken }}
{{- with $root.Values.imagePullSecrets }}
imagePullSecrets:
  {{- toYaml . | nindent 2 }}
{{- end }}
{{- with $root.Values.podSecurityContext }}
securityContext:
  {{- toYaml . | nindent 2 }}
{{- end }}
{{- with $root.Values.nodeSelector }}
nodeSelector:
  {{- toYaml . | nindent 2 }}
{{- end }}
{{- with $root.Values.tolerations }}
tolerations:
  {{- toYaml . | nindent 2 }}
{{- end }}
{{- if and (eq $component "server") (gt (int (include "service.minReplicas" $root)) 1) }}
{{- with $root.Values.topologySpreadConstraints }}
{{- $selector := dict "matchLabels" (include "service.selectorLabels" $root | fromYaml) }}
{{- $constraints := list }}
{{- range . }}
{{- $constraints = append $constraints (merge (dict) . (dict "labelSelector" $selector)) }}
{{- end }}
topologySpreadConstraints:
  {{- toYaml $constraints | nindent 2 }}
{{- end }}
{{- end }}
{{- end }}

{{/*
Projected ServiceAccount token volumes ([service_account] tokens) and their
read-only mounts in the server container.
*/}}
{{- define "service.saTokenVolumes" -}}
{{- range .Values.serviceAccount.tokenVolumes }}
- name: {{ .name }}
  projected:
    sources:
      {{- range .sources }}
      - serviceAccountToken:
          audience: {{ .audience | quote }}
          expirationSeconds: {{ .expirationSeconds }}
          path: {{ .path | quote }}
      {{- end }}
{{- end }}
{{- end }}

{{- define "service.saTokenVolumeMounts" -}}
{{- range .Values.serviceAccount.tokenVolumes }}
- name: {{ .name }}
  mountPath: {{ .mountPath }}
  readOnly: true
{{- end }}
{{- end }}

{{/*
Volumes of the service's own pods and the mounts of its server container:
the datastore CA mounts, the [[volumes]] and the projected ServiceAccount
tokens.
*/}}
{{- define "service.serverVolumes" -}}
{{- include "service.podVolumes" . }}
{{- include "service.saTokenVolumes" . }}
{{- end }}

{{- define "service.serverVolumeMounts" -}}
{{- include "service.volumeMounts" (dict "root" . "pvc" true) }}
{{- include "service.saTokenVolumeMounts" . }}
{{- end }}

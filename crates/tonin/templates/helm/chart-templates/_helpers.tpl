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

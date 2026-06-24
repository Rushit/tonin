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

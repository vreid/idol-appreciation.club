{{- /* Image ref: digest-pinned when digest is provided, else tag. */ -}}
{{- define "idol-canvas.image" -}}
{{- $i := index . "image" -}}
{{- if $i.digest -}}
{{ $i.repository }}@{{ $i.digest }}
{{- else -}}
{{ $i.repository }}:{{ $i.tag }}
{{- end -}}
{{- end -}}

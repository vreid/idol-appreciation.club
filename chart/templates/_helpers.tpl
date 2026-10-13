{{- /* Image ref: digest-pinned when a well-formed digest is provided, else
     tag. Fails at RENDER time on empty or malformed values — an invalid ref
     must never reach the cluster (InvalidImageName incident, 2026-10-13:
     a dispatch deploy with skipped image builds rolled malformed refs).
     Never emits tag and digest concatenated: digest replaces tag. */ -}}
{{- define "idol-canvas.image" -}}
{{- $i := index . "image" -}}
{{- $n := index . "name" -}}
{{- if $i.digest -}}
{{- if not (regexMatch "^[A-Za-z0-9_+.-]+:[0-9a-fA-F]{32,}$" $i.digest) -}}
{{- fail (printf "images.%s.digest %q is not an algorithm-prefixed digest (expected e.g. sha256:<hex>); refusing to render an invalid image ref" $n $i.digest) }}
{{- end -}}
{{ $i.repository }}@{{ $i.digest }}
{{- else if $i.tag -}}
{{- if not (regexMatch "^[A-Za-z0-9_][A-Za-z0-9_.-]{0,127}$" $i.tag) -}}
{{- fail (printf "images.%s.tag %q is not a valid image tag; refusing to render an invalid image ref" $n $i.tag) }}
{{- end -}}
{{ $i.repository }}:{{ $i.tag }}
{{- else -}}
{{- fail (printf "images.%s has neither digest nor tag; refusing to render an invalid image ref" $n) }}
{{- end -}}
{{- end -}}

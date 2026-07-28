{{/*
Return the proper Kora image name.
The tag defaults to "v{appVersion}" (e.g. v0.5.0); override image.tag to pin explicitly.
*/}}
{{- define "kora.image" -}}
{{- $imageRoot := dict "registry" .Values.image.registry "repository" .Values.image.repository "tag" (.Values.image.tag | default (printf "v%s" .Chart.AppVersion)) "digest" .Values.image.digest -}}
{{ include "common.images.image" (dict "imageRoot" $imageRoot "global" .Values.global) }}
{{- end -}}

{{/*
Return the proper Docker Image Registry Secret Names.
*/}}
{{- define "kora.imagePullSecrets" -}}
{{- include "common.images.renderPullSecrets" (dict "images" (list .Values.image) "context" $) -}}
{{- end -}}

{{/*
Create the name of the service account to use.
*/}}
{{- define "kora.serviceAccountName" -}}
{{- if .Values.serviceAccount.create -}}
    {{ default (include "common.names.fullname" .) .Values.serviceAccount.name }}
{{- else -}}
    {{ default "default" .Values.serviceAccount.name }}
{{- end -}}
{{- end -}}

{{/*
Return the name of the secret containing the database password.
Uses existingSecret if provided, otherwise returns the auto-created secret name.
*/}}
{{- define "kora.databaseSecretName" -}}
{{- if .Values.database.existingSecret -}}
{{- include "common.tplvalues.render" (dict "value" .Values.database.existingSecret "context" $) -}}
{{- else -}}
{{- printf "%s-db" (include "common.names.fullname" .) -}}
{{- end -}}
{{- end -}}

{{/*
Return the database password key — same in the auto-Secret and in existingSecret.
*/}}
{{- define "kora.databasePasswordKey" -}}
{{- print .Values.database.secretKeys.password -}}
{{- end -}}

{{/*
Returns "true" when the DATABASE_URL is mounted instead of the DB_* components.
URL mode is active when either database.url is set, or database.existingSecret is
set together with database.secretKeys.url.
*/}}
{{- define "kora.databaseUrlMode" -}}
{{- if or .Values.database.url (and .Values.database.existingSecret .Values.database.secretKeys.url) -}}
true
{{- end -}}
{{- end -}}

{{/*
Return the database URL key — same in the auto-Secret and in existingSecret.
Defaults to DATABASE_URL when secretKeys.url is empty (only reached if url mode is active via plain database.url).
*/}}
{{- define "kora.databaseUrlKey" -}}
{{- default "DATABASE_URL" .Values.database.secretKeys.url -}}
{{- end -}}

{{/*
Generic helper: fail if neither a direct value nor an existingSecret is provided.
Usage: include "kora.requireValueOrExistingSecret" (dict "value" .Values.foo.bar "existingSecret" .Values.foo.existingSecret "message" "Either foo.bar or foo.existingSecret must be provided")
*/}}
{{- define "kora.requireValueOrExistingSecret" -}}
{{- if and (not .value) (not .existingSecret) -}}
{{- fail .message -}}
{{- end -}}
{{- end -}}

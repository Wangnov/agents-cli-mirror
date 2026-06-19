#!/usr/bin/env bash
# Upload the pure mirror artifact tree to IHEP (S3-compatible object storage).
#
# Required env:
#   SECONDARY_S3_ENDPOINT
#   SECONDARY_S3_BUCKET
#   SECONDARY_S3_ACCESS_KEY_ID
#   SECONDARY_S3_SECRET_ACCESS_KEY
#
# Optional env:
#   SECONDARY_S3_REGION        default: us-east-1
#   PROVIDERS                  default: codex claude
#   IMMUTABLE_CACHE_CONTROL    default: public, max-age=31536000, immutable
#   SHORT_CACHE_CONTROL        default: no-cache
#   INSTALL_CACHE_CONTROL      default: no-cache
#   SECONDARY_S3_UPLOAD_ATTEMPTS           default: 4
#   SECONDARY_S3_CONNECT_TIMEOUT_SECONDS   default: 10
#   SECONDARY_S3_READ_TIMEOUT_SECONDS      default: 60
#   SECONDARY_S3_RETRY_SLEEP_SECONDS       default: 5
#
# Usage:
#   scripts/sync-secondary-s3.sh <artifacts-root>
set -euo pipefail

if [[ $# -ne 1 ]]; then
  echo "Usage: sync-secondary-s3.sh <artifacts-root>" >&2
  exit 2
fi

missing_env=""
for env_name in SECONDARY_S3_ENDPOINT SECONDARY_S3_BUCKET SECONDARY_S3_ACCESS_KEY_ID SECONDARY_S3_SECRET_ACCESS_KEY; do
  eval "env_value=\${$env_name:-}"
  if [[ -z "$env_value" ]]; then
    missing_env="$missing_env $env_name"
  fi
done

if [[ -n "$missing_env" ]]; then
  echo "warning: secondary S3 is not configured; skipping upload. Missing:$missing_env" >&2
  exit 0
fi

if ! command -v aws >/dev/null 2>&1; then
  echo "aws CLI is required for secondary S3 uploads." >&2
  exit 1
fi

if [[ ! -d "$1" ]]; then
  echo "Artifacts root is not a directory: $1" >&2
  exit 2
fi

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
artifacts_root="$(cd "$1" && pwd)"
endpoint="$SECONDARY_S3_ENDPOINT"
bucket="$SECONDARY_S3_BUCKET"
region="${SECONDARY_S3_REGION:-us-east-1}"
providers="${PROVIDERS:-codex claude}"
immutable_cache="${IMMUTABLE_CACHE_CONTROL:-public, max-age=31536000, immutable}"
short_cache="${SHORT_CACHE_CONTROL:-no-cache}"
install_cache="${INSTALL_CACHE_CONTROL:-no-cache}"
upload_attempts="${SECONDARY_S3_UPLOAD_ATTEMPTS:-4}"
connect_timeout="${SECONDARY_S3_CONNECT_TIMEOUT_SECONDS:-10}"
read_timeout="${SECONDARY_S3_READ_TIMEOUT_SECONDS:-60}"
retry_sleep="${SECONDARY_S3_RETRY_SLEEP_SECONDS:-5}"

if [[ -z "$bucket" || -z "$providers" ]]; then
  echo "SECONDARY_S3_BUCKET and PROVIDERS must not be empty." >&2
  exit 2
fi

require_positive_int() {
  local name="$1"
  local value="$2"

  if [[ ! "$value" =~ ^[1-9][0-9]*$ ]]; then
    echo "$name must be a positive integer, got: $value" >&2
    exit 2
  fi
}

require_positive_int SECONDARY_S3_UPLOAD_ATTEMPTS "$upload_attempts"
require_positive_int SECONDARY_S3_CONNECT_TIMEOUT_SECONDS "$connect_timeout"
require_positive_int SECONDARY_S3_READ_TIMEOUT_SECONDS "$read_timeout"
require_positive_int SECONDARY_S3_RETRY_SLEEP_SECONDS "$retry_sleep"

tmp_config="$(mktemp "${TMPDIR:-/tmp}/agents-cli-secondary-s3.XXXXXX")"
cleanup() {
  rm -f "$tmp_config"
}
trap cleanup EXIT

cat > "$tmp_config" <<EOF
[default]
region = $region
s3 =
    addressing_style = path
EOF

export AWS_ACCESS_KEY_ID="$SECONDARY_S3_ACCESS_KEY_ID"
export AWS_SECRET_ACCESS_KEY="$SECONDARY_S3_SECRET_ACCESS_KEY"
export AWS_DEFAULT_REGION="$region"
export AWS_EC2_METADATA_DISABLED=true
export AWS_CONFIG_FILE="$tmp_config"

content_type_for() {
  local name="$1"
  case "$name" in
    *.json) printf '%s' 'application/json' ;;
    *.sh) printf '%s' 'text/x-shellscript' ;;
    *.ps1) printf '%s' 'text/plain' ;;
    *.tar.gz|*.tgz) printf '%s' 'application/gzip' ;;
    *.zip) printf '%s' 'application/zip' ;;
    *) printf '%s' 'application/octet-stream' ;;
  esac
}

upload_file() {
  local file="$1"
  local key="${2#/}"
  local cache_control="$3"
  local content_type
  local attempt
  local delay
  local rc

  if [[ ! -f "$file" ]]; then
    echo "Not a file: $file" >&2
    exit 1
  fi
  if [[ -z "$key" ]]; then
    echo "Object key must not be empty." >&2
    exit 2
  fi

  content_type="$(content_type_for "$file")"

  attempt=1
  delay="$retry_sleep"
  while true; do
    echo "upload s3://$bucket/$key (attempt $attempt/$upload_attempts)"
    if aws s3 cp "$file" "s3://$bucket/$key" \
      --endpoint-url "$endpoint" \
      --region "$region" \
      --content-type "$content_type" \
      --cache-control "$cache_control" \
      --cli-connect-timeout "$connect_timeout" \
      --cli-read-timeout "$read_timeout" \
      --no-progress; then
      return 0
    else
      rc="$?"
    fi

    if ((attempt >= upload_attempts)); then
      echo "upload failed after $attempt attempts: s3://$bucket/$key" >&2
      return "$rc"
    fi

    echo "::warning:: secondary S3 upload failed for s3://$bucket/$key (exit $rc); retrying in ${delay}s" >&2
    sleep "$delay"
    attempt=$((attempt + 1))
    delay=$((delay * 2))
  done
}

validate_artifact_relpath() {
  local rel="$1"
  local provider="$2"
  local rest
  local version
  local platform_key
  local file_name

  rest="${rel#"$provider"/}"
  version="${rest%%/*}"
  if [[ -z "$version" || "$version" == "$rest" ]]; then
    echo "Malformed artifact path, expected $provider/<version>/<key>/<file>: $rel" >&2
    exit 2
  fi

  rest="${rest#*/}"
  platform_key="${rest%%/*}"
  if [[ -z "$platform_key" || "$platform_key" == "$rest" ]]; then
    echo "Malformed artifact path, expected $provider/<version>/<key>/<file>: $rel" >&2
    exit 2
  fi

  file_name="${rest#*/}"
  if [[ -z "$file_name" || "$file_name" == */* ]]; then
    echo "Malformed artifact path, expected $provider/<version>/<key>/<file>: $rel" >&2
    exit 2
  fi
}

upload_provider_artifacts() {
  local provider="$1"
  local provider_dir="$artifacts_root/$provider"
  local latest_file="$provider_dir/latest.json"

  if [[ ! -f "$latest_file" ]]; then
    echo "Missing required manifest: $latest_file" >&2
    exit 2
  fi

  LC_ALL=C find "$provider_dir" -type f | LC_ALL=C sort | while IFS= read -r file; do
    local rel
    rel="${file#"$artifacts_root"/}"
    if [[ "$rel" == "$provider/latest.json" ]]; then
      continue
    fi
    validate_artifact_relpath "$rel" "$provider"
    upload_file "$file" "$rel" "$immutable_cache"
  done

  upload_file "$latest_file" "$provider/latest.json" "$short_cache"
}

upload_installers() {
  local provider
  local ext
  local file

  for provider in $providers; do
    for ext in sh ps1; do
      file="$repo_root/install/$provider.$ext"
      if [[ ! -f "$file" ]]; then
        echo "Missing installer source: $file" >&2
        exit 2
      fi
      upload_file "$file" "$provider/install.$ext" "$install_cache"
    done
  done
}

seen_provider=0
for provider in $providers; do
  if [[ -d "$artifacts_root/$provider" ]]; then
    seen_provider=1
    upload_provider_artifacts "$provider"
  fi
done

if [[ "$seen_provider" -eq 0 ]]; then
  echo "No provider artifact directories found under $artifacts_root." >&2
  exit 2
fi

upload_installers

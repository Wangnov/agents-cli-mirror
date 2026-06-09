#!/usr/bin/env bash
# Upload the pure mirror artifact tree to R2 (Cloudflare object storage).
#
# Required env:
#   R2_S3_ENDPOINT
#   AWS_ACCESS_KEY_ID
#   AWS_SECRET_ACCESS_KEY
#
# Optional env:
#   R2_BUCKET                  default: agentclimirror
#   AWS_DEFAULT_REGION         default: auto
#   PROVIDERS                  default: codex claude
#   IMMUTABLE_CACHE_CONTROL    default: public, max-age=31536000, immutable
#   SHORT_CACHE_CONTROL        default: no-cache
#   INSTALL_CACHE_CONTROL      default: no-cache
#
# Usage:
#   scripts/sync-r2.sh <artifacts-root>
set -euo pipefail

: "${R2_S3_ENDPOINT:?R2_S3_ENDPOINT must be set}"
: "${AWS_ACCESS_KEY_ID:?AWS_ACCESS_KEY_ID must be set}"
: "${AWS_SECRET_ACCESS_KEY:?AWS_SECRET_ACCESS_KEY must be set}"

if [[ $# -ne 1 ]]; then
  echo "Usage: sync-r2.sh <artifacts-root>" >&2
  exit 2
fi

if ! command -v aws >/dev/null 2>&1; then
  echo "aws CLI is required for R2 uploads." >&2
  exit 1
fi

if [[ ! -d "$1" ]]; then
  echo "Artifacts root is not a directory: $1" >&2
  exit 2
fi

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
artifacts_root="$(cd "$1" && pwd)"
bucket="${R2_BUCKET:-agentclimirror}"
region="${AWS_DEFAULT_REGION:-auto}"
providers="${PROVIDERS:-codex claude}"
immutable_cache="${IMMUTABLE_CACHE_CONTROL:-public, max-age=31536000, immutable}"
short_cache="${SHORT_CACHE_CONTROL:-no-cache}"
install_cache="${INSTALL_CACHE_CONTROL:-no-cache}"

if [[ -z "$bucket" || -z "$providers" ]]; then
  echo "R2_BUCKET and PROVIDERS must not be empty." >&2
  exit 2
fi

tmp_config="$(mktemp "${TMPDIR:-/tmp}/agents-cli-r2.XXXXXX")"
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

export AWS_CONFIG_FILE="$tmp_config"
export AWS_DEFAULT_REGION="$region"
export AWS_EC2_METADATA_DISABLED=true

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

  if [[ ! -f "$file" ]]; then
    echo "Not a file: $file" >&2
    exit 1
  fi
  if [[ -z "$key" ]]; then
    echo "Object key must not be empty." >&2
    exit 2
  fi

  content_type="$(content_type_for "$file")"

  echo "upload s3://$bucket/$key"
  aws s3 cp "$file" "s3://$bucket/$key" \
    --endpoint-url "$R2_S3_ENDPOINT" \
    --region "$region" \
    --content-type "$content_type" \
    --cache-control "$cache_control" \
    --no-progress
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

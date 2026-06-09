#!/usr/bin/env bash
# Stateless R2-authoritative prune for the pure mirror object layout.
#
# Required env for R2 (Cloudflare object storage):
#   R2_S3_ENDPOINT
#   AWS_ACCESS_KEY_ID
#   AWS_SECRET_ACCESS_KEY
#
# Optional env:
#   R2_BUCKET                  default: agentclimirror
#   AWS_DEFAULT_REGION         default: auto
#   PROVIDERS                  default: codex claude
#   PRUNE_GRACE_DAYS           default: 0
#
# Optional env for IHEP (S3-compatible object storage):
#   SECONDARY_S3_ENDPOINT
#   SECONDARY_S3_BUCKET
#   SECONDARY_S3_ACCESS_KEY_ID
#   SECONDARY_S3_SECRET_ACCESS_KEY
#   SECONDARY_S3_REGION        default: us-east-1
set -euo pipefail

: "${R2_S3_ENDPOINT:?R2_S3_ENDPOINT must be set}"
: "${AWS_ACCESS_KEY_ID:?AWS_ACCESS_KEY_ID must be set}"
: "${AWS_SECRET_ACCESS_KEY:?AWS_SECRET_ACCESS_KEY must be set}"

if ! command -v aws >/dev/null 2>&1; then
  echo "aws CLI is required for pruning." >&2
  exit 1
fi

r2_endpoint="$R2_S3_ENDPOINT"
r2_bucket="${R2_BUCKET:-agentclimirror}"
r2_region="${AWS_DEFAULT_REGION:-auto}"
r2_access_key="$AWS_ACCESS_KEY_ID"
r2_secret_key="$AWS_SECRET_ACCESS_KEY"
providers="${PROVIDERS:-codex claude}"
grace_days="${PRUNE_GRACE_DAYS:-0}"

if [[ -z "$r2_bucket" || -z "$providers" ]]; then
  echo "R2_BUCKET and PROVIDERS must not be empty." >&2
  exit 2
fi
if [[ ! "$grace_days" =~ ^[0-9]+$ ]]; then
  echo "PRUNE_GRACE_DAYS must be a non-negative integer." >&2
  exit 2
fi

tmp_files=""
cleanup() {
  local file
  for file in $tmp_files; do
    rm -f "$file"
  done
}
trap cleanup EXIT

versions_file="$(mktemp "${TMPDIR:-/tmp}/agents-cli-prune-versions.XXXXXX")"
tmp_files="$tmp_files $versions_file"

make_backend_config() {
  local region="$1"

  backend_config="$(mktemp "${TMPDIR:-/tmp}/agents-cli-prune-aws.XXXXXX")"
  tmp_files="$tmp_files $backend_config"
  cat > "$backend_config" <<EOF
[default]
region = $region
s3 =
    addressing_style = path
EOF
}

use_backend() {
  backend_name="$1"
  backend_endpoint="$2"
  backend_bucket="$3"
  backend_region="$4"
  backend_access_key="$5"
  backend_secret_key="$6"

  make_backend_config "$backend_region"
  export AWS_ACCESS_KEY_ID="$backend_access_key"
  export AWS_SECRET_ACCESS_KEY="$backend_secret_key"
  export AWS_DEFAULT_REGION="$backend_region"
  export AWS_EC2_METADATA_DISABLED=true
  export AWS_CONFIG_FILE="$backend_config"
}

extract_version() {
  local json="$1"

  if command -v jq >/dev/null 2>&1; then
    printf '%s\n' "$json" | jq -r '.version // empty'
  elif command -v python3 >/dev/null 2>&1; then
    printf '%s\n' "$json" | python3 -c 'import json, sys; print((json.load(sys.stdin).get("version") or ""))'
  else
    printf '%s\n' "$json" | awk -F'"' '/"version"[[:space:]]*:/ { print $4; exit }'
  fi
}

object_epoch() {
  local lastmod="$1"
  local ts
  local epoch

  if [[ -z "$lastmod" || "$lastmod" == "None" ]]; then
    printf '%s' '0'
    return
  fi

  ts="${lastmod%%.*}"
  ts="${ts%%+*}"
  ts="${ts%Z}"

  if epoch="$(date -u -d "$lastmod" +%s 2>/dev/null)"; then
    printf '%s' "$epoch"
    return
  fi
  if epoch="$(date -u -jf '%Y-%m-%dT%H:%M:%S' "$ts" +%s 2>/dev/null)"; then
    printf '%s' "$epoch"
    return
  fi

  printf '%s' '0'
}

read_r2_versions() {
  local provider
  local latest_json
  local version

  : > "$versions_file"
  use_backend "R2" "$r2_endpoint" "$r2_bucket" "$r2_region" "$r2_access_key" "$r2_secret_key"

  for provider in $providers; do
    echo "read s3://$r2_bucket/$provider/latest.json"
    if ! latest_json="$(aws s3 cp "s3://$r2_bucket/$provider/latest.json" - \
      --endpoint-url "$r2_endpoint" \
      --region "$r2_region")"; then
      echo "Failed to read R2 latest.json for provider: $provider" >&2
      exit 1
    fi

    version="$(extract_version "$latest_json")"
    if [[ -z "$version" ]]; then
      echo "R2 latest.json for $provider is missing .version" >&2
      exit 1
    fi

    printf '%s %s\n' "$provider" "$version" >> "$versions_file"
  done
}

keep_key() {
  local key="$1"
  local provider="$2"
  local current_version="$3"

  if [[ "$key" == "$provider/latest.json" ]]; then
    return 0
  fi
  if [[ "$key" == "$provider/install.sh" || "$key" == "$provider/install.ps1" ]]; then
    return 0
  fi
  if [[ "$key" == "$provider/$current_version/"* ]]; then
    return 0
  fi

  return 1
}

prune_current_backend() {
  local provider
  local current_version
  local key
  local lastmod
  local rest
  local obj_epoch
  local cutoff_epoch

  cutoff_epoch=$(( $(date -u +%s) - grace_days * 86400 ))

  while read -r provider current_version; do
    [[ -z "$provider" || -z "$current_version" ]] && continue

    echo "list $backend_name s3://$backend_bucket/$provider/"
    aws s3api list-objects-v2 \
      --bucket "$backend_bucket" \
      --prefix "$provider/" \
      --endpoint-url "$backend_endpoint" \
      --region "$backend_region" \
      --query 'Contents[].[Key,LastModified]' \
      --output text | while read -r key lastmod rest; do
      [[ -z "$key" || "$key" == "None" ]] && continue
      [[ "$key" == */ ]] && continue

      if keep_key "$key" "$provider" "$current_version"; then
        continue
      fi

      obj_epoch="$(object_epoch "$lastmod")"
      if [[ "$obj_epoch" -eq 0 ]]; then
        echo "skip $backend_name s3://$backend_bucket/$key (unparseable timestamp: $lastmod)" >&2
        continue
      fi
      if [[ "$grace_days" -gt 0 && "$obj_epoch" -ge "$cutoff_epoch" ]]; then
        continue
      fi

      echo "delete $backend_name s3://$backend_bucket/$key"
      aws s3 rm "s3://$backend_bucket/$key" \
        --endpoint-url "$backend_endpoint" \
        --region "$backend_region" \
        --only-show-errors
    done
  done < "$versions_file"
}

read_r2_versions
use_backend "R2" "$r2_endpoint" "$r2_bucket" "$r2_region" "$r2_access_key" "$r2_secret_key"
prune_current_backend

secondary_any=0
secondary_missing=""
for env_name in SECONDARY_S3_ENDPOINT SECONDARY_S3_BUCKET SECONDARY_S3_ACCESS_KEY_ID SECONDARY_S3_SECRET_ACCESS_KEY; do
  eval "env_value=\${$env_name:-}"
  if [[ -n "$env_value" ]]; then
    secondary_any=1
  else
    secondary_missing="$secondary_missing $env_name"
  fi
done

if [[ "$secondary_any" -eq 1 ]]; then
  if [[ -n "$secondary_missing" ]]; then
    echo "warning: secondary S3 prune skipped. Missing:$secondary_missing" >&2
  else
    use_backend "secondary" \
      "$SECONDARY_S3_ENDPOINT" \
      "$SECONDARY_S3_BUCKET" \
      "${SECONDARY_S3_REGION:-us-east-1}" \
      "$SECONDARY_S3_ACCESS_KEY_ID" \
      "$SECONDARY_S3_SECRET_ACCESS_KEY"
    prune_current_backend
  fi
fi

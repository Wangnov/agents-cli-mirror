#!/usr/bin/env bash
set -euo pipefail

usage() {
  echo "usage: scripts/download-codex.sh <path-to-codex-latest.json> <artifacts-root>" >&2
}

die() {
  echo "::error:: $*" >&2
  exit 1
}

need_cmd() {
  command -v "$1" >/dev/null 2>&1 || die "missing required command: $1"
}

if [ "$#" -ne 2 ]; then
  usage
  exit 2
fi

manifest="$1"
artifacts_root="${2%/}"

[ -f "$manifest" ] || die "manifest not found: $manifest"
[ -n "$artifacts_root" ] || die "artifacts root must not be empty"

need_cmd curl
need_cmd python3
need_cmd awk

if command -v sha256sum >/dev/null 2>&1; then
  sha_tool="sha256sum"
elif command -v shasum >/dev/null 2>&1; then
  sha_tool="shasum"
else
  die "missing required command: sha256sum or shasum"
fi

sha256_file() {
  if [ "$sha_tool" = "sha256sum" ]; then
    sha256sum "$1" | awk '{print $1}'
  else
    shasum -a 256 "$1" | awk '{print $1}'
  fi
}

tmp_parse="$(mktemp "${TMPDIR:-/tmp}/download-codex.XXXXXX")"
tmp_download=""

cleanup() {
  if [ -n "${tmp_parse:-}" ]; then
    rm -f "$tmp_parse"
  fi
  if [ -n "${tmp_download:-}" ]; then
    rm -f "$tmp_download"
  fi
  return 0
}
trap cleanup EXIT

python3 - "$manifest" > "$tmp_parse" <<'PY'
import json
import sys

path = sys.argv[1]

def fail(message):
    sys.exit(f"::error:: {message}")

def safe_component(value, field):
    if not isinstance(value, str) or not value:
        fail(f"manifest field {field} must be a non-empty string")
    if "/" in value or "\0" in value or value in (".", ".."):
        fail(f"manifest field {field} is not a safe path component: {value!r}")
    return value

try:
    with open(path, "r") as f:
        manifest = json.load(f)
except Exception as exc:
    fail(f"failed to read manifest {path}: {exc}")

if manifest.get("provider") != "codex":
    fail(f"manifest provider must be codex, got {manifest.get('provider')!r}")

version = safe_component(manifest.get("version"), "version")
platforms = manifest.get("platforms")
if not isinstance(platforms, dict) or not platforms:
    fail("manifest platforms must be a non-empty object")

print(f"VERSION\t{version}")

for triple, entry in platforms.items():
    triple = safe_component(triple, "platform triple")
    if not isinstance(entry, dict):
        fail(f"manifest platform {triple} must be an object")
    file_name = safe_component(entry.get("file"), f"{triple}.file")
    sha256 = entry.get("sha256")
    if not isinstance(sha256, str) or len(sha256) != 64:
        fail(f"manifest platform {triple} has an invalid sha256")
    try:
        int(sha256, 16)
    except ValueError:
        fail(f"manifest platform {triple} has a non-hex sha256")
    size = entry.get("size")
    if not isinstance(size, int) or size < 0:
        fail(f"manifest platform {triple} has an invalid size")
    print(f"PLATFORM\t{triple}\t{file_name}\t{sha256.lower()}\t{size}")
PY

version=""
codex_root=""
files_total=0
downloaded=0
skipped=0
bytes_total=0

while IFS=$'\t' read -r kind triple file_name expected_sha declared_size; do
  if [ "$kind" = "VERSION" ]; then
    version="$triple"
    codex_root="${artifacts_root}/codex"
    mkdir -p "$codex_root"
    continue
  fi

  [ "$kind" = "PLATFORM" ] || die "unexpected parser row: $kind"
  [ -n "$version" ] || die "manifest version was not parsed before platforms"

  dest_dir="${artifacts_root}/codex/${version}/${triple}"
  dest="${dest_dir}/${file_name}"
  mkdir -p "$dest_dir"

  if [ -f "$dest" ]; then
    current_sha="$(sha256_file "$dest")"
    if [ "$current_sha" = "$expected_sha" ]; then
      echo "skip ${triple}/${file_name}"
      skipped=$((skipped + 1))
      files_total=$((files_total + 1))
      actual_size="$(wc -c < "$dest" | awk '{print $1}')"
      bytes_total=$((bytes_total + actual_size))
      continue
    fi
  fi

  url="https://github.com/openai/codex/releases/download/${version}/${file_name}"
  tmp_download="${dest}.tmp.$$"
  rm -f "$tmp_download"

  echo "download ${triple}/${file_name}"
  if ! curl -fL --retry 3 --retry-delay 2 -o "$tmp_download" "$url"; then
    rm -f "$tmp_download"
    tmp_download=""
    die "download failed: $url"
  fi

  actual_sha="$(sha256_file "$tmp_download")"
  if [ "$actual_sha" != "$expected_sha" ]; then
    rm -f "$tmp_download"
    tmp_download=""
    die "sha256 mismatch for ${triple}/${file_name}: expected ${expected_sha}, got ${actual_sha}"
  fi

  mv "$tmp_download" "$dest"
  tmp_download=""

  downloaded=$((downloaded + 1))
  files_total=$((files_total + 1))
  actual_size="$(wc -c < "$dest" | awk '{print $1}')"
  bytes_total=$((bytes_total + actual_size))
done < "$tmp_parse"

[ -n "$version" ] || die "manifest version was not found"
[ "$files_total" -gt 0 ] || die "manifest contains no platform files"

cp "$manifest" "${codex_root}/latest.json"

total_mb="$(awk -v bytes="$bytes_total" 'BEGIN { printf "%.1f", bytes / 1024 / 1024 }')"
echo "codex ${version}: ${files_total} files, ${total_mb} MB (downloaded ${downloaded}, skipped ${skipped})"

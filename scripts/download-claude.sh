#!/usr/bin/env bash
# Download Claude Code binaries described by claude/latest.json into a local
# artifact tree.
#
# Input manifest shape must match scripts/build-claude-manifest.sh:
# {"provider","version","published_at","platforms":{"<key>":{"file","sha256","size","bin"}}}
#
# Downloads are idempotent. Existing files with matching size and sha256 are
# kept. Missing or mismatched files are re-downloaded from:
# <claude upstream_url>/<version>/<key>/<file>
#
# Usage: download-claude.sh <path-to-claude-latest.json> <artifacts-root>
# CLAUDE_UPSTREAM_URL (optional) points at a compatible Claude Code GCS root.
set -euo pipefail

if [ "$#" -ne 2 ]; then
  echo "usage: download-claude.sh <path-to-claude-latest.json> <artifacts-root>" >&2
  exit 2
fi

manifest="$1"
artifacts_root="$2"
base="${CLAUDE_UPSTREAM_URL:-https://storage.googleapis.com/claude-code-dist-86c565f3-f756-42ad-8dfa-d59b1c096819/claude-code-releases}"
base="${base%/}"

if [ ! -f "$manifest" ]; then
  echo "::error:: manifest not found: $manifest" >&2
  exit 1
fi

tmpdir="$(mktemp -d "${TMPDIR:-/tmp}/claude-download.XXXXXX")"
cleanup() {
  rm -rf "$tmpdir"
}
trap cleanup EXIT

sha256_file() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | awk '{print $1}'
  elif command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$1" | awk '{print $1}'
  else
    echo "::error:: neither sha256sum nor shasum is available" >&2
    exit 1
  fi
}

file_size() {
  wc -c < "$1" | tr -d '[:space:]'
}

tasks="$tmpdir/tasks.tsv"
version_file="$tmpdir/version.txt"
python3 - "$manifest" "$tasks" "$version_file" <<'PY'
import json
import re
import sys

manifest_path, tasks_path, version_path = sys.argv[1:4]
with open(manifest_path, encoding="utf-8") as f:
    manifest = json.load(f)

if manifest.get("provider") != "claude":
    sys.exit("::error:: manifest provider is not claude")
version = manifest.get("version")
if not isinstance(version, str) or not version:
    sys.exit("::error:: manifest version is missing")
platforms = manifest.get("platforms")
if not isinstance(platforms, dict) or not platforms:
    sys.exit("::error:: manifest platforms is missing")

with open(version_path, "w", encoding="utf-8") as f:
    f.write(version)

with open(tasks_path, "w", encoding="utf-8") as f:
    for key, entry in platforms.items():
        if not isinstance(entry, dict):
            sys.exit(f"::error:: platform {key} is not an object")
        file_name = entry.get("file")
        sha256 = entry.get("sha256")
        size = entry.get("size")
        bin_name = entry.get("bin")
        if not isinstance(key, str) or "/" in key or not key:
            sys.exit(f"::error:: invalid platform key {key!r}")
        if not isinstance(file_name, str) or "/" in file_name or not file_name:
            sys.exit(f"::error:: invalid file for {key}")
        if not isinstance(bin_name, str) or not bin_name:
            sys.exit(f"::error:: invalid bin for {key}")
        if not isinstance(sha256, str) or not re.fullmatch(r"[0-9a-fA-F]{64}", sha256):
            sys.exit(f"::error:: invalid sha256 for {key}")
        if not isinstance(size, int) or size < 0:
            sys.exit(f"::error:: invalid size for {key}")
        f.write("\t".join([key, file_name, sha256.lower(), str(size), bin_name]) + "\n")
PY

version="$(cat "$version_file")"
tab="$(printf '\t')"
verified=0
downloaded=0

while IFS="$tab" read -r key file_name expected_sha expected_size bin_name; do
  [ -n "$key" ] || continue
  dest_dir="$artifacts_root/claude/$version/$key"
  dest="$dest_dir/$file_name"
  url="$base/$version/$key/$file_name"

  if [ -f "$dest" ]; then
    actual_size="$(file_size "$dest")"
    actual_sha="$(sha256_file "$dest")"
    if [ "$actual_size" = "$expected_size" ] && [ "$actual_sha" = "$expected_sha" ]; then
      echo "ok existing $key/$file_name"
      verified=$((verified + 1))
      continue
    fi
    echo "redownload $key/$file_name: checksum or size mismatch" >&2
  fi

  mkdir -p "$dest_dir"
  tmp_file="$(mktemp "$dest_dir/.${file_name}.XXXXXX")"
  if ! curl -fL --retry 3 --retry-delay 2 -o "$tmp_file" "$url"; then
    rm -f "$tmp_file"
    exit 1
  fi

  actual_size="$(file_size "$tmp_file")"
  actual_sha="$(sha256_file "$tmp_file")"
  if [ "$actual_size" != "$expected_size" ]; then
    rm -f "$tmp_file"
    echo "::error:: size mismatch for $key/$file_name: expected $expected_size, got $actual_size" >&2
    exit 1
  fi
  if [ "$actual_sha" != "$expected_sha" ]; then
    rm -f "$tmp_file"
    echo "::error:: sha256 mismatch for $key/$file_name: expected $expected_sha, got $actual_sha" >&2
    exit 1
  fi

  mv "$tmp_file" "$dest"
  echo "downloaded $key/$file_name"
  verified=$((verified + 1))
  downloaded=$((downloaded + 1))
done < "$tasks"

mkdir -p "$artifacts_root/claude"
cp "$manifest" "$artifacts_root/claude/latest.json"
echo "verified $verified claude artifacts for $version ($downloaded downloaded)"

#!/usr/bin/env bash
# Build claude/latest.json from the Claude Code GCS release bucket.
#
# Observed GCS protocol on 2026-06-09:
# - Latest discovery is a plain-text pointer at <upstream_url>/latest.
#   A separate <upstream_url>/stable pointer exists, but this pure mirror tracks
#   latest.
# - Per-version metadata lives at <upstream_url>/<version>/manifest.json.
#   It contains version, buildDate, and platforms.<key>.{binary,checksum,size}.
# - Per-platform binaries live at <upstream_url>/<version>/<key>/<binary>.
# - sha256 and size come from the upstream per-version manifest. If that manifest
#   is missing or incomplete, this script downloads each configured binary into a
#   temporary directory and computes sha256/size locally before emitting JSON.
#
# Output shape intentionally matches scripts/build-codex-manifest.sh exactly:
# {"provider","version","published_at","platforms":{"<key>":{"file","sha256","size","bin"}}}
#
# Usage: build-claude-manifest.sh <out.json>
# CLAUDE_UPSTREAM_URL (optional) points at a compatible Claude Code GCS root.
set -euo pipefail

if [ "$#" -ne 1 ]; then
  echo "usage: build-claude-manifest.sh <out.json>" >&2
  exit 2
fi

out="$1"
claude_upstream_url="${CLAUDE_UPSTREAM_URL:-https://storage.googleapis.com/claude-code-dist-86c565f3-f756-42ad-8dfa-d59b1c096819/claude-code-releases}"

tmpdir="$(mktemp -d "${TMPDIR:-/tmp}/claude-manifest.XXXXXX")"
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

config_json="$tmpdir/config.json"
python3 - "$claude_upstream_url" > "$config_json" <<'PY'
import json
import sys

upstream = sys.argv[1].rstrip("/")
platforms = [
    "darwin-x64",
    "darwin-arm64",
    "linux-x64",
    "linux-arm64",
    "linux-x64-musl",
    "linux-arm64-musl",
    "win32-x64",
    "win32-arm64",
]
file_by_platform = {
    key: "claude.exe" if key.startswith("win32-") else "claude"
    for key in platforms
}

print(json.dumps({
    "upstream_url": upstream,
    "platforms": platforms,
    "files": file_by_platform,
}))
PY

base="$(python3 - "$config_json" <<'PY'
import json
import sys
with open(sys.argv[1]) as f:
    print(json.load(f)["upstream_url"])
PY
)"

version="$(curl -fsSL --retry 3 --retry-delay 2 "$base/latest" | tr -d '\r' | awk '{$1=$1; print}')"
if [ -z "$version" ]; then
  echo "::error:: empty latest pointer from $base/latest" >&2
  exit 1
fi

mkdir -p "$(dirname "$out")"
out_tmp="$tmpdir/latest.json"
upstream_manifest="$tmpdir/upstream-manifest.json"
manifest_url="$base/$version/manifest.json"

if curl -fsSL --retry 3 --retry-delay 2 -o "$upstream_manifest" "$manifest_url"; then
  if python3 - "$config_json" "$upstream_manifest" "$out_tmp" "$version" <<'PY'
import json
import re
import sys

config_path, upstream_path, out_path, version = sys.argv[1:5]
with open(config_path) as f:
    config = json.load(f)
with open(upstream_path) as f:
    upstream = json.load(f)

if str(upstream.get("version")) != version:
    sys.exit(f"::error:: upstream manifest version {upstream.get('version')!r} does not match latest {version!r}")

upstream_platforms = upstream.get("platforms") or {}
platforms = {}
for key in config["platforms"]:
    entry = upstream_platforms.get(key)
    if not isinstance(entry, dict):
        sys.exit(f"::error:: upstream manifest missing platform {key}")
    file_name = config["files"][key]
    binary = entry.get("binary") or file_name
    if binary != file_name:
        sys.exit(f"::error:: config file {key}/{file_name} does not match upstream binary {binary}")
    sha256 = entry.get("checksum") or entry.get("sha256")
    size = entry.get("size")
    if not isinstance(sha256, str) or not re.fullmatch(r"[0-9a-fA-F]{64}", sha256):
        sys.exit(f"::error:: upstream manifest missing valid sha256 for {key}")
    if not isinstance(size, int) or size < 0:
        sys.exit(f"::error:: upstream manifest missing valid size for {key}")
    platforms[key] = {
        "file": file_name,
        "sha256": sha256.lower(),
        "size": size,
        "bin": binary,
    }

manifest = {
    "provider": "claude",
    "version": version,
    "published_at": upstream.get("buildDate"),
    "platforms": platforms,
}
with open(out_path, "w", encoding="utf-8") as f:
    json.dump(manifest, f, indent=2, ensure_ascii=False)
    f.write("\n")
PY
  then
    mv "$out_tmp" "$out"
    echo "wrote $out: claude $version, upstream manifest checksums"
    exit 0
  fi
  echo "::warning:: upstream manifest was incomplete; downloading files to compute sha256" >&2
else
  echo "::warning:: no upstream manifest at $manifest_url; downloading files to compute sha256" >&2
fi

tasks="$tmpdir/tasks.tsv"
python3 - "$config_json" > "$tasks" <<'PY'
import json
import sys

with open(sys.argv[1]) as f:
    config = json.load(f)

for key in config["platforms"]:
    file_name = config["files"][key]
    bin_name = "claude.exe" if key.startswith("win32-") else "claude"
    if file_name != bin_name:
        sys.exit(f"::error:: expected {key}/{bin_name}, got {key}/{file_name}")
    print("\t".join([key, file_name, bin_name]))
PY

computed="$tmpdir/computed.jsonl"
: > "$computed"
tab="$(printf '\t')"
while IFS="$tab" read -r key file_name bin_name; do
  [ -n "$key" ] || continue
  url="$base/$version/$key/$file_name"
  dest="$tmpdir/downloads/$key/$file_name"
  mkdir -p "$(dirname "$dest")"
  curl -fL --retry 3 --retry-delay 2 -o "$dest" "$url"
  sha="$(sha256_file "$dest")"
  size="$(file_size "$dest")"
  printf '{"key":"%s","file":"%s","sha256":"%s","size":%s,"bin":"%s"}\n' \
    "$key" "$file_name" "$sha" "$size" "$bin_name" >> "$computed"
done < "$tasks"

python3 - "$computed" "$upstream_manifest" "$out_tmp" "$version" <<'PY'
import json
import os
import sys

computed_path, upstream_path, out_path, version = sys.argv[1:5]
published_at = None
if os.path.exists(upstream_path):
    try:
        with open(upstream_path) as f:
            published_at = json.load(f).get("buildDate")
    except Exception:
        published_at = None

platforms = {}
with open(computed_path) as f:
    for line in f:
        if not line.strip():
            continue
        row = json.loads(line)
        platforms[row.pop("key")] = row

manifest = {
    "provider": "claude",
    "version": version,
    "published_at": published_at,
    "platforms": platforms,
}
with open(out_path, "w", encoding="utf-8") as f:
    json.dump(manifest, f, indent=2, ensure_ascii=False)
    f.write("\n")
PY

mv "$out_tmp" "$out"
echo "wrote $out: claude $version, locally computed checksums"

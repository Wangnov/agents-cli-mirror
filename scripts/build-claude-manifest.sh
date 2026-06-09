#!/usr/bin/env bash
# Build claude/latest.json from the Claude Code GCS release bucket.
#
# Observed protocol, confirmed against config.cloud.toml and GCS on 2026-06-09:
# - config.cloud.toml has provider name="claude", source="gcs_release", and
#   upstream_url=https://storage.googleapis.com/claude-code-dist-86c565f3-f756-42ad-8dfa-d59b1c096819/claude-code-releases
# - Latest discovery is a plain-text pointer at <upstream_url>/latest.
#   A separate <upstream_url>/stable pointer exists, but this pure mirror tracks
#   latest because the claude provider block declares tags=["latest"].
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
# CLAUDE_CONFIG (optional) points at an alternate config.cloud.toml path.
set -euo pipefail

if [ "$#" -ne 1 ]; then
  echo "usage: build-claude-manifest.sh <out.json>" >&2
  exit 2
fi

out="$1"
config="${CLAUDE_CONFIG:-config.cloud.toml}"

if [ ! -f "$config" ]; then
  echo "::error:: config not found: $config" >&2
  exit 1
fi

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
python3 - "$config" > "$config_json" <<'PY'
import ast
import json
import os
import re
import sys

path = sys.argv[1]

def strip_comment(line):
    in_quote = False
    escaped = False
    out = []
    for ch in line:
        if escaped:
            out.append(ch)
            escaped = False
            continue
        if ch == "\\" and in_quote:
            out.append(ch)
            escaped = True
            continue
        if ch == '"':
            in_quote = not in_quote
            out.append(ch)
            continue
        if ch == "#" and not in_quote:
            break
        out.append(ch)
    return "".join(out).strip()

providers = []
current = None
pending_key = None
pending_value = []

def finish_pending():
    global pending_key, pending_value
    if pending_key is not None:
        current[pending_key] = ast.literal_eval("\n".join(pending_value))
        pending_key = None
        pending_value = []

with open(path, "r", encoding="utf-8") as f:
    for raw in f:
        line = strip_comment(raw)
        if not line:
            continue
        if pending_key is not None:
            pending_value.append(line)
            if "]" in line:
                finish_pending()
            continue
        if line == "[[providers]]":
            if current is not None:
                providers.append(current)
            current = {}
            continue
        if current is None:
            continue
        if line.startswith("[") and line.endswith("]"):
            continue
        match = re.match(r"^([A-Za-z0-9_]+)\s*=\s*(.+)$", line)
        if not match:
            continue
        key, value = match.group(1), match.group(2).strip()
        if value.startswith("[") and "]" not in value:
            pending_key = key
            pending_value = [value]
            continue
        try:
            current[key] = ast.literal_eval(value)
        except Exception:
            current[key] = value

finish_pending()
if current is not None:
    providers.append(current)

claude = next((p for p in providers if p.get("name") == "claude"), None)
if claude is None:
    sys.exit("::error:: config has no [[providers]] block with name=\"claude\"")

upstream = claude.get("upstream_url")
platforms = claude.get("platforms") or []
files = claude.get("files") or []
if not upstream:
    sys.exit("::error:: claude provider has no upstream_url")
if not platforms:
    sys.exit("::error:: claude provider has no platforms")
if not files:
    sys.exit("::error:: claude provider has no files")

file_by_platform = {}
for rel in files:
    parts = rel.replace("\\", "/").split("/")
    if len(parts) != 2 or not parts[0] or not parts[1]:
        sys.exit(f"::error:: claude file path must look like <platform>/<file>: {rel}")
    file_by_platform[parts[0]] = parts[1]

missing = [p for p in platforms if p not in file_by_platform]
if missing:
    sys.exit("::error:: claude files missing platform entries: " + ", ".join(missing))

print(json.dumps({
    "upstream_url": upstream.rstrip("/"),
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

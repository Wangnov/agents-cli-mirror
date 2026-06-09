#!/usr/bin/env bash
# Build codex/latest.json from an openai/codex GitHub release.
#
# We mirror only the 6 installable CLI archives (one per platform family).
# The other release assets (codex-app-server, npm, wheels, .dmg, alt formats)
# are outside this pure CLI mirror. Linux gnu hosts use the musl build
# (static, portable), so 2 linux archives cover all 4 linux platforms.
#
# sha256 comes straight from GitHub's `asset.digest` ("sha256:<hex>"), so we
# never download the artifact just to hash it — the manifest is built from the
# release API response alone. The sync step verifies bytes against this on upload.
#
# Usage: build-codex-manifest.sh [<tag>] <out.json>
#   tag defaults to the latest non-prerelease (resolved via the GitHub API).
#   GH_TOKEN (optional) raises the API rate limit in CI.
set -euo pipefail

repo="openai/codex"

if [ "$#" -eq 1 ]; then
  tag=""; out="$1"
elif [ "$#" -eq 2 ]; then
  tag="$1"; out="$2"
else
  echo "usage: build-codex-manifest.sh [<tag>] <out.json>" >&2
  exit 2
fi

api="https://api.github.com/repos/${repo}/releases"
url="${api}/latest"
[ -n "$tag" ] && url="${api}/tags/${tag}"

auth=()
[ -n "${GH_TOKEN:-}" ] && auth=(-H "Authorization: Bearer ${GH_TOKEN}")

release_json="$(curl -fsSL --retry 3 --retry-delay 2 \
  -H "Accept: application/vnd.github+json" \
  -H "X-GitHub-Api-Version: 2022-11-28" \
  ${auth[@]+"${auth[@]}"} "$url")"

# `var=$(cmd)` does not trip `set -e` when cmd fails, so guard explicitly —
# otherwise an API hiccup would feed empty input to the parser below.
if [ -z "$release_json" ]; then
  echo "::error:: empty response from $url (GitHub API unreachable or rate-limited?)" >&2
  exit 1
fi

# Pass the release JSON via env, not stdin: the heredoc below already occupies
# python's stdin (it's the program source for `python3 -`).
RELEASE_JSON="$release_json" python3 - "$out" <<'PY'
import os, sys, json

out = sys.argv[1]
rel = json.loads(os.environ["RELEASE_JSON"])

# platform triple -> (canonical asset name, installed binary name).
# musl entries also serve the matching gnu host (see install script hints).
PLATFORMS = {
    "x86_64-apple-darwin":       ("codex-x86_64-apple-darwin.tar.gz",        "codex"),
    "aarch64-apple-darwin":      ("codex-aarch64-apple-darwin.tar.gz",       "codex"),
    "x86_64-unknown-linux-musl": ("codex-x86_64-unknown-linux-musl.tar.gz",  "codex"),
    "aarch64-unknown-linux-musl":("codex-aarch64-unknown-linux-musl.tar.gz", "codex"),
    "x86_64-pc-windows-msvc":    ("codex-x86_64-pc-windows-msvc.exe.zip",    "codex.exe"),
    "aarch64-pc-windows-msvc":   ("codex-aarch64-pc-windows-msvc.exe.zip",   "codex.exe"),
}

assets = {a["name"]: a for a in rel.get("assets", [])}
version = rel["tag_name"]

platforms = {}
for triple, (fname, binname) in PLATFORMS.items():
    a = assets.get(fname)
    if a is None:
        sys.exit(f"::error:: release {version} is missing required asset {fname}")
    digest = a.get("digest") or ""
    if not digest.startswith("sha256:"):
        sys.exit(f"::error:: asset {fname} has no sha256 digest (got {digest!r})")
    platforms[triple] = {
        "file": fname,
        "sha256": digest.split(":", 1)[1],
        "size": a["size"],
        "bin": binname,
    }

manifest = {
    "provider": "codex",
    "version": version,
    "published_at": rel.get("published_at"),
    # Each artifact lives at <mirror>/codex/<version>/<triple>/<file>; the worker
    # routes that path to R2 (global) or a presigned IHEP URL (CN).
    "platforms": platforms,
}

with open(out, "w") as f:
    json.dump(manifest, f, indent=2, ensure_ascii=False)
    f.write("\n")
print(f"wrote {out}: codex {version}, {len(platforms)} platforms")
PY

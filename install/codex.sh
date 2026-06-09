#!/usr/bin/env bash
set -euo pipefail

PROVIDER="codex"
DEFAULT_MIRROR_URL="https://install.agentsmirror.com"
MIRROR_URL="${MIRROR_URL:-$DEFAULT_MIRROR_URL}"
INSTALL_DIR="${INSTALL_DIR:-$HOME/.local/bin}"

MANIFEST_PROVIDER=""
MANIFEST_VERSION=""
ARTIFACT_FILE=""
ARTIFACT_SHA256=""
ARTIFACT_BIN=""
ARTIFACT_ARCHIVE_MEMBER=""

usage() {
    cat <<'EOF'
Usage: codex.sh [options]
  --mirror <url>       mirror base URL (default: https://install.agentsmirror.com)
  --install-dir <dir>  install directory (default: ~/.local/bin)
  -h, --help           show this help

Environment:
  MIRROR_URL           mirror base URL override
  INSTALL_DIR          install directory override
EOF
}

die() {
    echo "Error: $*" >&2
    exit 1
}

require_value() {
    local option="${1:-option}"

    [ "$#" -ge 2 ] || die "$option requires a value"
}

while [ "$#" -gt 0 ]; do
    case "$1" in
        --mirror|--mirror-url)
            require_value "$@"
            MIRROR_URL="$2"
            shift 2
            ;;
        --install-dir)
            require_value "$@"
            INSTALL_DIR="$2"
            shift 2
            ;;
        -h|--help)
            usage
            exit 0
            ;;
        *)
            die "Unknown option: $1"
            ;;
    esac
done

[ -n "$MIRROR_URL" ] || die "MIRROR_URL is empty"
MIRROR_URL="${MIRROR_URL%/}"

case "$INSTALL_DIR" in
    "~") INSTALL_DIR="$HOME" ;;
    "~/"*) INSTALL_DIR="$HOME/${INSTALL_DIR#~/}" ;;
esac

detect_platform() {
    local os arch
    os="$(uname -s | tr '[:upper:]' '[:lower:]')"
    arch="$(uname -m)"

    case "$os" in
        darwin)
            case "$arch" in
                x86_64) echo "x86_64-apple-darwin" ;;
                arm64|aarch64) echo "aarch64-apple-darwin" ;;
                *) die "Unsupported macOS architecture: $arch" ;;
            esac
            ;;
        linux)
            case "$arch" in
                x86_64|amd64) echo "x86_64-unknown-linux-musl" ;;
                arm64|aarch64) echo "aarch64-unknown-linux-musl" ;;
                *) die "Unsupported Linux architecture: $arch" ;;
            esac
            ;;
        *)
            die "Unsupported OS: $os"
            ;;
    esac
}

download_to() {
    local url="$1"
    local out="$2"

    if command -v curl >/dev/null 2>&1; then
        curl -fsSL --retry 3 --retry-delay 1 --connect-timeout 15 -o "$out" "$url"
    elif command -v wget >/dev/null 2>&1; then
        wget -q -O "$out" "$url"
    else
        die "Need curl or wget to download files"
    fi
}

sha256_file() {
    local path="$1"

    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$path" | awk '{print $1}'
    elif command -v shasum >/dev/null 2>&1; then
        shasum -a 256 "$path" | awk '{print $1}'
    else
        die "Need sha256sum or shasum to verify downloads"
    fi
}

lowercase() {
    printf '%s' "$1" | tr '[:upper:]' '[:lower:]'
}

parse_manifest_with_jq() {
    local manifest_path="$1"
    local platform_key="$2"

    MANIFEST_PROVIDER="$(jq -r '.provider // ""' "$manifest_path")"
    MANIFEST_VERSION="$(jq -r '.version // ""' "$manifest_path")"
    ARTIFACT_FILE="$(jq -r --arg key "$platform_key" '.platforms[$key].file // ""' "$manifest_path")"
    ARTIFACT_SHA256="$(jq -r --arg key "$platform_key" '.platforms[$key].sha256 // ""' "$manifest_path")"
    ARTIFACT_BIN="$(jq -r --arg key "$platform_key" '.platforms[$key].bin // ""' "$manifest_path")"
    ARTIFACT_ARCHIVE_MEMBER="$(jq -r --arg key "$platform_key" '.platforms[$key].archive_member // ""' "$manifest_path")"
}

parse_manifest_with_python() {
    local manifest_path="$1"
    local platform_key="$2"
    local parsed

    parsed="$(python3 - "$manifest_path" "$platform_key" <<'PY'
import json
import shlex
import sys

with open(sys.argv[1], "r", encoding="utf-8") as f:
    manifest = json.load(f)

entry = manifest.get("platforms", {}).get(sys.argv[2]) or {}
values = {
    "MANIFEST_PROVIDER": manifest.get("provider", ""),
    "MANIFEST_VERSION": manifest.get("version", ""),
    "ARTIFACT_FILE": entry.get("file", ""),
    "ARTIFACT_SHA256": entry.get("sha256", ""),
    "ARTIFACT_BIN": entry.get("bin", ""),
    "ARTIFACT_ARCHIVE_MEMBER": entry.get("archive_member", ""),
}
for key, value in values.items():
    print(f"{key}={shlex.quote(str(value))}")
PY
)"
    eval "$parsed"
}

parse_manifest_with_awk() {
    local manifest_path="$1"
    local platform_key="$2"
    local block

    MANIFEST_PROVIDER="$(awk -F'"' '/"provider"[[:space:]]*:/ { print $4; exit }' "$manifest_path")"
    MANIFEST_VERSION="$(awk -F'"' '/"version"[[:space:]]*:/ { print $4; exit }' "$manifest_path")"
    block="$(awk -v target="$platform_key" '
        index($0, "\"" target "\"") && $0 ~ /:[[:space:]]*\{/ { found = 1; next }
        found && /^[[:space:]]*}/ { exit }
        found { print }
    ' "$manifest_path")"
    ARTIFACT_FILE="$(printf '%s\n' "$block" | awk -F'"' '/"file"[[:space:]]*:/ { print $4; exit }')"
    ARTIFACT_SHA256="$(printf '%s\n' "$block" | awk -F'"' '/"sha256"[[:space:]]*:/ { print $4; exit }')"
    ARTIFACT_BIN="$(printf '%s\n' "$block" | awk -F'"' '/"bin"[[:space:]]*:/ { print $4; exit }')"
    ARTIFACT_ARCHIVE_MEMBER="$(printf '%s\n' "$block" | awk -F'"' '/"archive_member"[[:space:]]*:/ { print $4; exit }')"
}

parse_manifest() {
    local manifest_path="$1"
    local platform_key="$2"

    if command -v jq >/dev/null 2>&1; then
        parse_manifest_with_jq "$manifest_path" "$platform_key"
    elif command -v python3 >/dev/null 2>&1; then
        parse_manifest_with_python "$manifest_path" "$platform_key"
    else
        parse_manifest_with_awk "$manifest_path" "$platform_key"
    fi
}

extract_artifact() {
    local artifact_path="$1"
    local extract_dir="$2"
    local bin_name="$3"
    local archive_member="${4:-}"
    local found
    local file_count

    case "$artifact_path" in
        *.tar.gz|*.tgz)
            tar -xzf "$artifact_path" -C "$extract_dir"
            ;;
        *.zip)
            if command -v unzip >/dev/null 2>&1; then
                unzip -q "$artifact_path" -d "$extract_dir"
            elif command -v bsdtar >/dev/null 2>&1; then
                bsdtar -xf "$artifact_path" -C "$extract_dir"
            else
                die "Need unzip or bsdtar to extract zip archives"
            fi
            ;;
        *)
            printf '%s\n' "$artifact_path"
            return 0
            ;;
    esac

    if [ -n "$archive_member" ]; then
        found="$(find "$extract_dir" -type f -name "$archive_member" -print | sed -n '1p')"
        [ -n "$found" ] || die "Archive member '$archive_member' was not found after extraction"
    else
        found="$(find "$extract_dir" -type f -name "$bin_name" -print | sed -n '1p')"
        if [ -z "$found" ]; then
            file_count="$(find "$extract_dir" -type f -print | wc -l | tr -d '[:space:]')"
            if [ "$file_count" = "1" ]; then
                found="$(find "$extract_dir" -type f -print | sed -n '1p')"
            fi
        fi
        [ -n "$found" ] || die "Binary '$bin_name' was not found after extraction"
    fi
    printf '%s\n' "$found"
}

path_contains_dir() {
    case ":${PATH:-}:" in
        *":$1:"*) return 0 ;;
        *) return 1 ;;
    esac
}

PLATFORM_KEY="$(detect_platform)"
MANIFEST_URL="$MIRROR_URL/$PROVIDER/latest.json"
TMP_DIR="$(mktemp -d "${TMPDIR:-/tmp}/agents-${PROVIDER}.XXXXXX")"

cleanup() {
    rm -rf "$TMP_DIR"
}
trap cleanup EXIT

MANIFEST_PATH="$TMP_DIR/latest.json"
download_to "$MANIFEST_URL" "$MANIFEST_PATH"
parse_manifest "$MANIFEST_PATH" "$PLATFORM_KEY"

[ "$MANIFEST_PROVIDER" = "$PROVIDER" ] || die "Manifest provider is '$MANIFEST_PROVIDER', expected '$PROVIDER'"
[ -n "$MANIFEST_VERSION" ] || die "Manifest is missing version"
if [ -z "$ARTIFACT_FILE" ] || [ -z "$ARTIFACT_SHA256" ] || [ -z "$ARTIFACT_BIN" ]; then
    die "Mirror manifest does not support platform '$PLATFORM_KEY' for provider '$PROVIDER'"
fi

case "$ARTIFACT_FILE" in
    */*|"") die "Manifest artifact file is invalid: $ARTIFACT_FILE" ;;
esac
case "$ARTIFACT_ARCHIVE_MEMBER" in
    */*|*\\*) die "Manifest archive member is invalid: $ARTIFACT_ARCHIVE_MEMBER" ;;
esac

ARTIFACT_URL="$MIRROR_URL/$PROVIDER/$MANIFEST_VERSION/$PLATFORM_KEY/$ARTIFACT_FILE"
ARTIFACT_PATH="$TMP_DIR/$ARTIFACT_FILE"
download_to "$ARTIFACT_URL" "$ARTIFACT_PATH"

EXPECTED_SHA256="$(lowercase "$ARTIFACT_SHA256")"
ACTUAL_SHA256="$(lowercase "$(sha256_file "$ARTIFACT_PATH")")"
[ "$ACTUAL_SHA256" = "$EXPECTED_SHA256" ] || die "SHA256 mismatch for $ARTIFACT_FILE: expected $EXPECTED_SHA256, got $ACTUAL_SHA256"

EXTRACT_DIR="$TMP_DIR/extract"
mkdir -p "$EXTRACT_DIR"
BIN_PATH="$(extract_artifact "$ARTIFACT_PATH" "$EXTRACT_DIR" "$ARTIFACT_BIN" "$ARTIFACT_ARCHIVE_MEMBER")"
chmod +x "$BIN_PATH"

mkdir -p "$INSTALL_DIR"
INSTALL_PATH="$INSTALL_DIR/$ARTIFACT_BIN"
if command -v install >/dev/null 2>&1; then
    install -m 0755 "$BIN_PATH" "$INSTALL_PATH"
else
    cp "$BIN_PATH" "$INSTALL_PATH"
    chmod +x "$INSTALL_PATH"
fi

echo "Success: installed $PROVIDER $MANIFEST_VERSION to $INSTALL_PATH"
if ! path_contains_dir "$INSTALL_DIR"; then
    echo "$INSTALL_DIR is not on PATH."
    echo "Add it to PATH with:"
    echo "  export PATH=\"$INSTALL_DIR:\$PATH\""
fi

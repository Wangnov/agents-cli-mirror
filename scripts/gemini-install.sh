#!/bin/bash
set -e

MIRROR_URL="${MIRROR_URL:-__MIRROR_URL__}"
MIRROR_URL_OVERRIDE=""
INSTALL_DIR="${INSTALL_DIR:-$HOME/.agents}"
TAG="${TAG:-latest}"
VERSION=""
NODE_TAG="${NODE_TAG:-latest}"
NODE_VERSION=""
NODE_PTY_TAG="${NODE_PTY_TAG:-latest}"
NODE_PTY_VERSION=""
UPGRADE=false
CHECK_ONLY=false
NO_MODIFY_PATH=false
JSON=false

INSTALLER_TAG="${INSTALLER_TAG:-latest}"
INSTALLER_VERSION=""

usage() {
    cat <<'EOF'
Usage: gemini-install.sh [options]
  --tag <tag>              tag (default: latest)
  --version <version>      install a specific version
  --node-tag <tag>         node tag (default: latest)
  --node-version <ver>     node version
  --node-pty-tag <tag>     node-pty tag (default: latest)
  --node-pty-version <ver> node-pty version
  --upgrade                force reinstall
  --check                  check only
  --no-modify-path         do not modify PATH
  --json                   JSON output from installer
  --mirror-url <url>       override mirror url
  --installer-tag <tag>    installer tag (default: latest)
  --installer-version <v>  installer version
EOF
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --tag)
            TAG="$2"
            shift 2
            ;;
        --version)
            VERSION="$2"
            shift 2
            ;;
        --node-tag)
            NODE_TAG="$2"
            shift 2
            ;;
        --node-version)
            NODE_VERSION="$2"
            shift 2
            ;;
        --node-pty-tag)
            NODE_PTY_TAG="$2"
            shift 2
            ;;
        --node-pty-version)
            NODE_PTY_VERSION="$2"
            shift 2
            ;;
        --upgrade)
            UPGRADE=true
            shift
            ;;
        --check)
            CHECK_ONLY=true
            shift
            ;;
        --no-modify-path)
            NO_MODIFY_PATH=true
            shift
            ;;
        --json)
            JSON=true
            shift
            ;;
        --mirror-url)
            MIRROR_URL_OVERRIDE="$2"
            shift 2
            ;;
        --installer-tag)
            INSTALLER_TAG="$2"
            shift 2
            ;;
        --installer-version)
            INSTALLER_VERSION="$2"
            shift 2
            ;;
        -h|--help)
            usage
            exit 0
            ;;
        *)
            echo "Unknown option: $1" >&2
            usage
            exit 1
            ;;
    esac
done

if [[ -n "$MIRROR_URL_OVERRIDE" ]]; then
    MIRROR_URL="$MIRROR_URL_OVERRIDE"
fi

if [[ -z "$MIRROR_URL" || "$MIRROR_URL" == "__MIRROR_URL__" ]]; then
    echo "MIRROR_URL is not set" >&2
    exit 1
fi

detect_platform() {
    local os arch libc=""
    os=$(uname -s | tr '[:upper:]' '[:lower:]')
    arch=$(uname -m)
    case "$os" in
        darwin)
            case "$arch" in
                x86_64) echo "x86_64-apple-darwin" ;;
                arm64)  echo "aarch64-apple-darwin" ;;
                *) echo "Unsupported arch: $arch" >&2; exit 1 ;;
            esac
            ;;
        linux)
            if ldd --version 2>&1 | grep -q musl; then
                libc="-musl"
            else
                libc="-gnu"
            fi
            case "$arch" in
                x86_64)  echo "x86_64-unknown-linux${libc}" ;;
                aarch64) echo "aarch64-unknown-linux${libc}" ;;
                *) echo "Unsupported arch: $arch" >&2; exit 1 ;;
            esac
            ;;
        *)
            echo "Unsupported OS: $os" >&2
            exit 1
            ;;
    esac
}

extract_installer() {
    local archive="$1"
    local out_dir="$2"
    local bin_name="$3"

    case "$archive" in
        *.tar.xz)
            if command -v tar >/dev/null 2>&1; then
                set +e
                tar -xJf "$archive" -C "$out_dir" >/dev/null 2>&1
                tar_status=$?
                set -e
                if [[ "$tar_status" -ne 0 ]]; then
                    if command -v python3 >/dev/null 2>&1; then
                        python3 - "$archive" "$out_dir" <<'PY'
import sys
import tarfile
archive, out_dir = sys.argv[1:3]
with tarfile.open(archive, "r:*") as tf:
    tf.extractall(out_dir)
PY
                    else
                        echo "Failed to extract $archive (need tar with xz or python3)" >&2
                        exit 1
                    fi
                fi
            elif command -v python3 >/dev/null 2>&1; then
                python3 - "$archive" "$out_dir" <<'PY'
import sys
import tarfile
archive, out_dir = sys.argv[1:3]
with tarfile.open(archive, "r:*") as tf:
    tf.extractall(out_dir)
PY
            else
                echo "Failed to extract $archive (need tar with xz or python3)" >&2
                exit 1
            fi
            ;;
        *.zip)
            if command -v unzip >/dev/null 2>&1; then
                unzip -q "$archive" -d "$out_dir"
            elif command -v python3 >/dev/null 2>&1; then
                python3 - "$archive" "$out_dir" <<'PY'
import sys
import zipfile
archive, out_dir = sys.argv[1:3]
with zipfile.ZipFile(archive, "r") as zf:
    zf.extractall(out_dir)
PY
            else
                echo "Failed to extract $archive (need unzip or python3)" >&2
                exit 1
            fi
            ;;
        *)
            echo "$archive"
            return 0
            ;;
    esac

    local found
    found=$(find "$out_dir" -type f -name "$bin_name" -print -quit)
    if [[ -z "$found" ]]; then
        echo "Installer binary not found after extraction" >&2
        exit 1
    fi
    echo "$found"
}

PLATFORM=$(detect_platform)

if [[ -z "$INSTALLER_VERSION" ]]; then
    INSTALLER_VERSION=$(curl -fsSL -S --retry 3 --retry-delay 1 --connect-timeout 10 "$MIRROR_URL/installer/$INSTALLER_TAG")
fi

BIN_NAME="__INSTALLER_BIN__"
TMP_DIR=$(mktemp -d)
TMP_BIN="$TMP_DIR/$BIN_NAME"
cleanup() { rm -rf "$TMP_DIR"; }
trap cleanup EXIT

CHECKSUM_URL="$MIRROR_URL/installer/$INSTALLER_VERSION/$PLATFORM/checksum.txt"
CHECKSUM_LINE=$(curl -fsSL -S --retry 3 --retry-delay 1 --connect-timeout 10 "$CHECKSUM_URL")
EXPECTED_SHA256=$(echo "$CHECKSUM_LINE" | awk '{print $1}')
ARCHIVE_NAME=$(echo "$CHECKSUM_LINE" | awk '{print $2}')
if [[ -z "$ARCHIVE_NAME" ]]; then
    echo "Failed to resolve installer filename" >&2
    exit 1
fi
TMP_ARCHIVE="$TMP_DIR/$ARCHIVE_NAME"

curl -fsSL -S --retry 3 --retry-delay 1 --connect-timeout 10 \
    "$MIRROR_URL/installer/$INSTALLER_VERSION/$PLATFORM/$ARCHIVE_NAME" \
    -o "$TMP_ARCHIVE"

if command -v sha256sum &> /dev/null; then
    ACTUAL_SHA256=$(sha256sum "$TMP_ARCHIVE" | awk '{print $1}')
elif command -v shasum &> /dev/null; then
    ACTUAL_SHA256=$(shasum -a 256 "$TMP_ARCHIVE" | awk '{print $1}')
else
    ACTUAL_SHA256=""
fi

if [[ -n "$EXPECTED_SHA256" && -n "$ACTUAL_SHA256" && "$EXPECTED_SHA256" != "$ACTUAL_SHA256" ]]; then
    echo "Checksum mismatch: expected $EXPECTED_SHA256, got $ACTUAL_SHA256" >&2
    exit 1
fi

TMP_BIN=$(extract_installer "$TMP_ARCHIVE" "$TMP_DIR" "$BIN_NAME")
chmod +x "$TMP_BIN"

ARGS=(gemini --tag "$TAG")
if [[ -n "$VERSION" ]]; then
    ARGS+=(--version "$VERSION")
fi
if [[ -n "$NODE_TAG" ]]; then
    ARGS+=(--node-tag "$NODE_TAG")
fi
if [[ -n "$NODE_VERSION" ]]; then
    ARGS+=(--node-version "$NODE_VERSION")
fi
if [[ -n "$NODE_PTY_TAG" ]]; then
    ARGS+=(--node-pty-tag "$NODE_PTY_TAG")
fi
if [[ -n "$NODE_PTY_VERSION" ]]; then
    ARGS+=(--node-pty-version "$NODE_PTY_VERSION")
fi
if $UPGRADE; then
    ARGS+=(--upgrade)
fi
if $CHECK_ONLY; then
    ARGS+=(--check)
fi
if $NO_MODIFY_PATH; then
    ARGS+=(--no-modify-path)
fi
if $JSON; then
    ARGS+=(--json)
fi

exec "$TMP_BIN" --mirror-url "$MIRROR_URL" --install-dir "$INSTALL_DIR" "${ARGS[@]}"

#!/bin/bash
set -e

INSTALL_DIR="${INSTALL_DIR:-$HOME/.agents}"
BIN_DIR="$INSTALL_DIR/bin"

detect_lang() {
    local lang="${LC_ALL:-${LC_MESSAGES:-${LANG:-}}}"

    # macOS: fallback to AppleLocale if LANG is empty or C/POSIX
    if [[ -z "$lang" || "$lang" == "C" || "$lang" == "C.UTF-8" || "$lang" == "POSIX" ]]; then
        if command -v defaults &>/dev/null; then
            lang=$(defaults read -g AppleLocale 2>/dev/null || true)
        fi
    fi

    if [[ "$lang" == zh* ]]; then
        echo "zh"
    else
        echo "en"
    fi
}

LANG_CODE=$(detect_lang)

msg() {
    local key="$1"
    shift
    case "$LANG_CODE" in
        zh)
            case "$key" in
                "uninstalling") printf "正在卸载 Codex...\n" ;;
                "removed")      printf "已删除: %s\n" "$1" ;;
                "complete")     printf "卸载完成!\n" ;;
            esac
            ;;
        *)
            case "$key" in
                "uninstalling") printf "Uninstalling Codex...\n" ;;
                "removed")      printf "Removed: %s\n" "$1" ;;
                "complete")     printf "Uninstallation complete!\n" ;;
            esac
            ;;
    esac
}

remove_version_key() {
    local key="$1"
    local version_file="$INSTALL_DIR/versions.json"
    if [[ ! -f "$version_file" ]]; then
        return
    fi

    if command -v jq &> /dev/null; then
        local tmp_json
        tmp_json="$(mktemp)"
        jq "del(.\"$key\")" "$version_file" > "$tmp_json" && mv "$tmp_json" "$version_file"
    elif command -v python3 &> /dev/null; then
        python3 - "$version_file" "$key" <<'PY'
import json
import sys
path, key = sys.argv[1:3]
with open(path, "r", encoding="utf-8") as f:
    data = json.load(f)
data.pop(key, None)
with open(path, "w", encoding="utf-8") as f:
    json.dump(data, f, indent=2)
PY
    elif command -v python &> /dev/null; then
        python - "$version_file" "$key" <<'PY'
import json
import sys
path, key = sys.argv[1:3]
with open(path, "r", encoding="utf-8") as f:
    data = json.load(f)
data.pop(key, None)
with open(path, "w", encoding="utf-8") as f:
    json.dump(data, f, indent=2)
PY
    fi
}

msg "uninstalling"

if [[ -f "$BIN_DIR/codex" ]]; then
    rm -f "$BIN_DIR/codex"
    msg "removed" "$BIN_DIR/codex"
fi

if [[ -L "$HOME/.local/bin/codex" ]]; then
    rm -f "$HOME/.local/bin/codex"
    msg "removed" "$HOME/.local/bin/codex"
fi

remove_version_key "codex"

if [[ -d "$BIN_DIR" ]] && [[ -z "$(ls -A "$BIN_DIR")" ]]; then
    rmdir "$BIN_DIR" 2>/dev/null || true
fi

if [[ -d "$INSTALL_DIR" ]] && [[ -z "$(ls -A "$INSTALL_DIR")" ]]; then
    rmdir "$INSTALL_DIR" 2>/dev/null || true
fi

msg "complete"

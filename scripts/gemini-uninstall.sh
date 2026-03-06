#!/bin/bash
set -e

INSTALL_DIR="${INSTALL_DIR:-$HOME/.agents}"
BIN_DIR="$INSTALL_DIR/bin"

REMOVE_NODE_PTY="ask"
REMOVE_NODE="ask"

while [[ $# -gt 0 ]]; do
    case "$1" in
        --yes)
            REMOVE_NODE_PTY="yes"
            REMOVE_NODE="yes"
            shift
            ;;
        --remove-node-pty)
            REMOVE_NODE_PTY="yes"
            shift
            ;;
        --remove-node)
            REMOVE_NODE="yes"
            shift
            ;;
        --no-node-pty|--keep-node-pty)
            REMOVE_NODE_PTY="no"
            shift
            ;;
        --no-node|--keep-node)
            REMOVE_NODE="no"
            shift
            ;;
        *)
            echo "Unknown option: $1" >&2
            exit 1
            ;;
    esac
done

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
                "uninstalling")        printf "正在卸载 Gemini CLI...\n" ;;
                "removed")             printf "已删除: %s\n" "$1" ;;
                "remove_node_pty")     printf "是否同时卸载 node-pty 预编译? [y/N] " ;;
                "remove_node")         printf "是否同时卸载私有 Node.js? [y/N] " ;;
                "complete")            printf "卸载完成!\n" ;;
            esac
            ;;
        *)
            case "$key" in
                "uninstalling")        printf "Uninstalling Gemini CLI...\n" ;;
                "removed")             printf "Removed: %s\n" "$1" ;;
                "remove_node_pty")     printf "Remove node-pty prebuilds as well? [y/N] " ;;
                "remove_node")         printf "Remove private Node.js as well? [y/N] " ;;
                "complete")            printf "Uninstallation complete!\n" ;;
            esac
            ;;
    esac
}

prompt_yes_no() {
    local prompt="$1"
    local answer=""
    if [[ -r /dev/tty ]]; then
        read -r -p "$prompt" answer </dev/tty || true
    else
        read -r -p "$prompt" answer || true
    fi
    [[ "$answer" =~ ^[Yy]$ ]]
}

should_remove() {
    local mode="$1"
    local prompt_key="$2"
    case "$mode" in
        yes) return 0 ;;
        no)  return 1 ;;
        *)   prompt_yes_no "$(msg "$prompt_key")" ;;
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

# Remove Gemini wrapper and files
if [[ -f "$BIN_DIR/gemini" ]]; then
    rm -f "$BIN_DIR/gemini"
    msg "removed" "$BIN_DIR/gemini"
fi

if [[ -d "$INSTALL_DIR/gemini" ]]; then
    rm -rf "$INSTALL_DIR/gemini"
    msg "removed" "$INSTALL_DIR/gemini"
fi

if [[ -L "$HOME/.local/bin/gemini" ]]; then
    rm -f "$HOME/.local/bin/gemini"
    msg "removed" "$HOME/.local/bin/gemini"
fi

remove_version_key "gemini"

if should_remove "$REMOVE_NODE_PTY" "remove_node_pty"; then
    if [[ -d "$INSTALL_DIR/node-pty" ]]; then
        rm -rf "$INSTALL_DIR/node-pty"
        msg "removed" "$INSTALL_DIR/node-pty"
    fi
    remove_version_key "node_pty"
fi

if should_remove "$REMOVE_NODE" "remove_node"; then
    if [[ -d "$INSTALL_DIR/node" ]]; then
        rm -rf "$INSTALL_DIR/node"
        msg "removed" "$INSTALL_DIR/node"
    fi
    remove_version_key "node"
fi

msg "complete"

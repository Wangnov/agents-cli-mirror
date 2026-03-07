#!/usr/bin/env bash
set -euo pipefail

if [[ -z "${MIRROR_URL:-}" ]]; then
  echo "MIRROR_URL is required" >&2
  exit 1
fi
export MIRROR_URL

is_musl() {
  if command -v ldd >/dev/null 2>&1; then
    if ldd --version 2>&1 | grep -qi musl; then
      return 0
    fi
  fi
  if ls /lib/ld-musl-* >/dev/null 2>&1; then
    return 0
  fi
  if ls /lib/libc.musl-* >/dev/null 2>&1; then
    return 0
  fi
  return 1
}

tui_check() {
  local bin="$1"
  if command -v python3 >/dev/null 2>&1; then
    BIN="$bin" python3 - <<'PY'
import os
import pty
import signal
import sys
import time

bin_path = os.environ["BIN"]
pid, _ = pty.fork()
if pid == 0:
    os.execv(bin_path, [bin_path])

time.sleep(2)
pid_out, _ = os.waitpid(pid, os.WNOHANG)
if pid_out != 0:
    sys.exit(1)

try:
    os.kill(pid, signal.SIGTERM)
except ProcessLookupError:
    sys.exit(0)

time.sleep(0.5)
pid_out, _ = os.waitpid(pid, os.WNOHANG)
if pid_out == 0:
    try:
        os.kill(pid, signal.SIGKILL)
    except ProcessLookupError:
        pass
sys.exit(0)
PY
  elif command -v timeout >/dev/null 2>&1 && command -v script >/dev/null 2>&1; then
    set +e
    timeout 5s script -q -c "$bin" /dev/null
    code=$?
    set -e
    if [[ "$code" -ne 124 && "$code" -ne 137 && "$code" -ne 143 ]]; then
      return 1
    fi
  else
    echo "No tool available for TUI check (need python3 or timeout+script)" >&2
    return 1
  fi
}

run_cli() {
  local name="$1"
  local cmd="$2"
  local uninstall_args="${3:-}"
  local bin_path="$HOME/.agents/bin/$cmd"

  echo "==> Installing $name"
  curl -fsSL "$MIRROR_URL/install/$name" >/dev/null
  curl -fsSL "$MIRROR_URL/install/$name" | MIRROR_URL="$MIRROR_URL" bash -s -- --install-dir "$HOME/.agents" --no-modify-path

  echo "==> Version check: $cmd"
  "$bin_path" --version || "$bin_path" -V

  if [[ "$cmd" == "codex" ]]; then
    echo "==> Help check: $cmd"
    "$bin_path" --help >/dev/null
  else
    echo "==> TUI check: $cmd"
    tui_check "$bin_path"
  fi

  echo "==> Uninstalling $name"
  curl -fsSL "$MIRROR_URL/uninstall/$name" >/dev/null
  if [[ -n "$uninstall_args" ]]; then
    curl -fsSL "$MIRROR_URL/uninstall/$name" | MIRROR_URL="$MIRROR_URL" bash -s -- $uninstall_args
  else
    curl -fsSL "$MIRROR_URL/uninstall/$name" | MIRROR_URL="$MIRROR_URL" bash -s --
  fi

  if [[ -e "$HOME/.agents/bin/$cmd" ]]; then
    echo "Uninstall check failed: $cmd still exists" >&2
    exit 1
  fi
}

if [[ "${SKIP_CLAUDE:-}" == "1" ]]; then
  echo "Skipping claude-code: SKIP_CLAUDE=1"
else
  run_cli "claude-code" "claude-code"
fi
if [[ "${SKIP_CODEX:-}" == "1" ]]; then
  echo "Skipping codex: SKIP_CODEX=1"
else
  run_cli "codex" "codex"
fi
if [[ "${SKIP_GEMINI:-}" == "1" ]]; then
  echo "Skipping gemini: SKIP_GEMINI=1"
elif is_musl; then
  echo "Skipping gemini on musl: Node.js runtime not available"
else
  run_cli "gemini" "gemini" "--yes"
fi

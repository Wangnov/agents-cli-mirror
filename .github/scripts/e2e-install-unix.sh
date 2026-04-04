#!/usr/bin/env bash
set -euo pipefail

if [[ -z "${MIRROR_URL:-}" ]]; then
  echo "MIRROR_URL is required" >&2
  exit 1
fi
export MIRROR_URL
PATH_MODE="${ACM_E2E_PATH_MODE:-shim}"
BASE_PATH="${PATH:-/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin}"
export SHELL="${SHELL:-/bin/bash}"

setup_path_mode() {
  export PATH="$BASE_PATH"

  case "$PATH_MODE" in
    shim)
      mkdir -p "$HOME/.local/bin"
      export PATH="${HOME}/.local/bin:$PATH"
      ;;
    reload)
      ;;
    *)
      echo "Unknown ACM_E2E_PATH_MODE: $PATH_MODE" >&2
      exit 1
      ;;
  esac
}

reload_shell_path() {
  local shell_name=""
  local -a rc_candidates=()
  shell_name="$(basename "${SHELL:-}")"

  case "$shell_name" in
    zsh)
      rc_candidates+=("$HOME/.zshrc" "$HOME/.zprofile" "$HOME/.profile")
      ;;
    bash)
      rc_candidates+=("$HOME/.bashrc" "$HOME/.profile")
      ;;
    *)
      rc_candidates+=("$HOME/.profile" "$HOME/.bashrc" "$HOME/.zshrc" "$HOME/.zprofile")
      ;;
  esac

  for rc_file in "${rc_candidates[@]}"; do
    if [[ ! -f "$rc_file" ]]; then
      continue
    fi

    set +u
    # shellcheck disable=SC1090
    source "$rc_file"
    set -u
    hash -r
    return
  done

  echo "No shell rc file created for reload mode" >&2
  exit 1
}

is_musl() {
  if command -v ldd >/dev/null 2>&1; then
    local ldd_output=""
    ldd_output="$(ldd --version 2>&1 || true)"
    if printf '%s' "$ldd_output" | grep -qi musl; then
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
  local public_name="$1"
  local cmd="$2"
  local install_root="$HOME/.acm"
  local bin_path="$HOME/.agents/bin/$cmd"
  local shim_path="$HOME/.local/bin/$cmd"
  local install_url="$MIRROR_URL/$public_name/install.sh"
  local uninstall_url="$MIRROR_URL/$public_name/uninstall.sh"
  shift 2
  local uninstall_args=("$@")

  setup_path_mode

  echo "==> Installing $public_name ($cmd) [mode=$PATH_MODE]"
  curl -fsSL "$install_url" >/dev/null
  curl -fsSL "$install_url" | MIRROR_URL="$MIRROR_URL" bash -s --

  echo "==> Resolve check: $cmd"
  resolved_bin=""
  if [[ "$PATH_MODE" == "reload" ]]; then
    reload_shell_path
  fi
  resolved_bin="$(command -v "$cmd" || true)"
  if [[ -z "$resolved_bin" ]]; then
    echo "PATH check failed: $cmd not found" >&2
    exit 1
  fi
  if [[ "$resolved_bin" != "$shim_path" && "$resolved_bin" != "$bin_path" ]]; then
    echo "PATH check failed: $cmd resolved to unexpected path $resolved_bin" >&2
    exit 1
  fi

  if [[ "$cmd" == "codex" ]]; then
    echo "==> Help check: $cmd"
    "$cmd" --help >/dev/null
  else
    echo "==> Version check: $cmd"
    "$cmd" --version || "$cmd" -V

    echo "==> TUI check: $cmd"
    tui_check "$resolved_bin"
  fi

  test -f "$install_root/state.toml"

  echo "==> Uninstalling $public_name ($cmd)"
  curl -fsSL "$uninstall_url" >/dev/null
  if [[ ${#uninstall_args[@]} -gt 0 ]]; then
    curl -fsSL "$uninstall_url" | MIRROR_URL="$MIRROR_URL" bash -s -- "${uninstall_args[@]}"
  else
    curl -fsSL "$uninstall_url" | MIRROR_URL="$MIRROR_URL" bash -s --
  fi

  if [[ -e "$bin_path" || -L "$bin_path" || -e "$shim_path" || -L "$shim_path" || -e "$install_root/bin/$cmd" || -L "$install_root/bin/$cmd" ]]; then
    echo "Uninstall check failed: $cmd still exists" >&2
    exit 1
  fi
}

if [[ "${SKIP_CLAUDE:-}" == "1" ]]; then
  echo "Skipping claude: SKIP_CLAUDE=1"
elif is_musl; then
  echo "Skipping claude on musl: upstream binary is currently incompatible"
else
  run_cli "claude" "claude"
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
  run_cli "gemini" "gemini"
fi

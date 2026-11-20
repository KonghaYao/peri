#!/usr/bin/env bash
# Launches the real rivet engine with a smaller log-shard quota.
#
# rivetkit's native layer injects `RIVET__STORAGE__LOG_SHARD__MAX_DISK_BYTES=4294967296` right
# before spawning the engine, so a host-process environment variable is overwritten. This wrapper
# is the last process before the engine and is the only reliable place to change the quota. The
# default 4 GiB refuses to start on machines with less free disk; 512 MiB is ample for local chats.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

case "$(uname -s)-$(uname -m)" in
  Darwin-arm64) PACKAGE="engine-cli-darwin-arm64" ;;
  Darwin-x86_64) PACKAGE="engine-cli-darwin-x64" ;;
  Linux-x86_64) PACKAGE="engine-cli-linux-x64-musl" ;;
  Linux-aarch64) PACKAGE="engine-cli-linux-arm64-musl" ;;
  *)
    echo "rivet-engine: unsupported platform $(uname -s)/$(uname -m)" >&2
    exit 1
    ;;
esac

# Resolve from the installed dependency graph, not a package-local node_modules: consumers may
# hoist @rivetkit/<platform> to the project root or nest it under another package.
find_binary() {
  if [ -n "${PERI_RIVET_ENGINE_BINARY:-}" ] && [ -x "$PERI_RIVET_ENGINE_BINARY" ]; then
    printf '%s\n' "$PERI_RIVET_ENGINE_BINARY"
    return 0
  fi
  local dir="$SCRIPT_DIR"
  while :; do
    local candidate="$dir/node_modules/@rivetkit/$PACKAGE/rivet-engine"
    if [ -x "$candidate" ]; then
      printf '%s\n' "$candidate"
      return 0
    fi
    [ "$dir" = "/" ] && break
    dir="$(dirname "$dir")"
  done
  return 1
}

BINARY="$(find_binary || true)"
if [ -z "$BINARY" ]; then
  echo "rivet-engine: @rivetkit/$PACKAGE is missing; install it or set PERI_RIVET_ENGINE_BINARY" >&2
  exit 1
fi

unset RIVET__STORAGE__LOG_SHARD__MAX_DISK_BYTES RIVET__STORAGE__COLD_STORE__DISK_CACHE_BYTES
export RIVET__STORAGE__LOG_SHARD__MAX_DISK_BYTES="${PERI_RIVET_LOG_SHARD_MAX_DISK_BYTES:-536870912}"
export RIVET__STORAGE__COLD_STORE__DISK_CACHE_BYTES="${PERI_RIVET_COLD_STORE_DISK_CACHE_BYTES:-0}"

# Record the PID for the host's last-resort cleanup. `exec` below keeps this PID, and the host
# appends one line per launch so a failed start retry can still be cleaned up. rivetkit spawns this
# wrapper directly from the API process, so PPID is the host; the file name therefore matches the
# host's `host-<pid>.pid` lookup. Nothing is ever matched by process name.
if [ -n "${RIVETKIT_STORAGE_PATH:-}" ]; then
  ENGINE_PID_FILE="${RIVETKIT_STORAGE_PATH%/}/.rivetkit/var/engine/host-${PPID}.pid"
  if ! { mkdir -p "$(dirname "$ENGINE_PID_FILE")" && printf '%s\n' "$$" >>"$ENGINE_PID_FILE"; }; then
    echo "rivet-engine: could not record the engine pid for host cleanup" >&2
  fi
fi

exec "$BINARY" "$@"

#!/usr/bin/env bash
# Starts the API with a workspace-relative rivet data directory.
#
# The native engine reads RIVETKIT_STORAGE_PATH at process start, so it cannot be defaulted from
# TypeScript after rivetkit is imported. Without this export the engine would fall back to
# ~/.rivetkit and could collide with a user-owned engine.
#
# Precedence: shell environment > repository-root .env > .data/rivet. Bun loads .env by itself,
# but a value exported here always wins, so the default must resolve the file first.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

# Prints RIVETKIT_STORAGE_PATH from a dotenv file without sourcing it. Supported subset:
# `KEY=value`, an optional `export ` prefix, single/double quotes, and trailing `#` comments.
# Unsupported or unrelated lines are ignored; escapes are not interpreted.
env_file_storage_path() {
  local file="$1"
  local line rest value
  local result="" found=""
  [ -f "$file" ] || return 1
  while IFS= read -r line || [ -n "$line" ]; do
    line="${line%$'\r'}"
    # Trim leading whitespace so `  KEY=value` and `  # comment` are recognized.
    line="${line#"${line%%[![:space:]]*}"}"
    case "$line" in
      '' | '#'*) continue ;;
      export[[:space:]]*)
        line="${line#export}"
        line="${line#"${line%%[![:space:]]*}"}"
        ;;
    esac
    case "$line" in
      RIVETKIT_STORAGE_PATH=* | RIVETKIT_STORAGE_PATH[[:space:]]=*) ;;
      *) continue ;;
    esac

    value="${line#RIVETKIT_STORAGE_PATH}"
    value="${value#"${value%%[![:space:]]*}"}" # spaces before `=`
    value="${value#=}"
    value="${value#"${value%%[![:space:]]*}"}" # spaces after `=`

    case "$value" in
      \'*)
        rest="${value#\'}"
        case "$rest" in
          *\'*) value="${rest%%\'*}" ; rest="${rest#*\'}" ;;
          *) continue ;;
        esac
        ;;
      \"*)
        rest="${value#\"}"
        case "$rest" in
          *\"*) value="${rest%%\"*}" ; rest="${rest#*\"}" ;;
          *) continue ;;
        esac
        ;;
      *)
        value="${value%%[[:space:]]#*}"
        value="${value%"${value##*[![:space:]]}"}"
        rest=""
        ;;
    esac

    # After a quoted value only whitespace or a comment may follow.
    if [ -n "${rest:-}" ]; then
      rest="${rest#"${rest%%[![:space:]]*}"}"
      case "$rest" in
        '' | '#'*) ;;
        *) continue ;;
      esac
    fi
    result="$value"
    found=1
  done <"$file"
  [ -n "$found" ] || return 1
  printf '%s\n' "$result"
  return 0
}

STORAGE_PATH="${RIVETKIT_STORAGE_PATH:-}"
if [ -z "$STORAGE_PATH" ] && [ -f "$ROOT/.env" ]; then
  if value="$(env_file_storage_path "$ROOT/.env")"; then
    STORAGE_PATH="$value"
  fi
fi
STORAGE_PATH="${STORAGE_PATH:-.data/rivet}"

case "$STORAGE_PATH" in
  /*) ;;
  *) STORAGE_PATH="$ROOT/$STORAGE_PATH" ;;
esac
export RIVETKIT_STORAGE_PATH="$STORAGE_PATH"

mkdir -p "$RIVETKIT_STORAGE_PATH"
chmod 700 "$RIVETKIT_STORAGE_PATH"

exec bun "$@"

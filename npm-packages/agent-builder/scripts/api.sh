#!/usr/bin/env bash
# Starts the API with a workspace-relative rivet data directory.
#
# The native engine reads RIVETKIT_STORAGE_PATH at process start, so it cannot be defaulted from
# TypeScript after rivetkit is imported. Without this export the engine would fall back to
# ~/.rivetkit and could collide with a user-owned engine.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

STORAGE_PATH="${RIVETKIT_STORAGE_PATH:-.data/rivet}"
case "$STORAGE_PATH" in
  /*) ;;
  *) STORAGE_PATH="$ROOT/$STORAGE_PATH" ;;
esac
export RIVETKIT_STORAGE_PATH="$STORAGE_PATH"

mkdir -p "$RIVETKIT_STORAGE_PATH"
chmod 700 "$RIVETKIT_STORAGE_PATH"

exec bun "$@"

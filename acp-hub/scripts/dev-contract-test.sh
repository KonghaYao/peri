#!/bin/bash
# dev.sh 的静态装配契约。真实进程 readiness 由 product/e2e 与人工启动探针覆盖；
# 这里防止端口、日志权限和进程清理重新漂移。
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
DEV="${ROOT}/dev.sh"

require() {
    local PATTERN="$1"
    local MESSAGE="$2"
    if ! grep -Fq -- "${PATTERN}" "${DEV}"; then
        echo "FAIL: ${MESSAGE}" >&2
        exit 1
    fi
}

forbid() {
    local PATTERN="$1"
    local MESSAGE="$2"
    if grep -Fq -- "${PATTERN}" "${DEV}"; then
        echo "FAIL: ${MESSAGE}" >&2
        exit 1
    fi
}

bash -n "${DEV}"
require 'LISTEN_PORT="${ACP_HUB_LISTEN_PORT:-8456}"' 'listen port must have one authoritative shell value'
require 'INSTANCE_SERVER_URL="${ACP_HUB_SERVER_URL:-ws://${LISTEN_ADDR}:${LISTEN_PORT}/instance}"' 'instance URL must derive from the same listen address/port'
require 'lsof -nP -iTCP:"${LISTEN_PORT}" -sTCP:LISTEN' 'occupied ports must fail before rebuilding or spawning'
require '--listen "${LISTEN_ADDR}"' 'server launch must receive the authoritative listen address'
require '--listen-port "${LISTEN_PORT}"' 'server launch must receive the authoritative listen port'
require '--config-dir "${CONFIG_DIR}"' 'server launch must receive the authoritative config directory'
require '--data-dir "${DATA_DIR}"' 'server launch must receive the authoritative data directory'
require '--server-url "${INSTANCE_SERVER_URL}"' 'instance launch must receive the derived URL'
require 'umask 077' 'runtime logs and credentials must default to private permissions'
require 'server.${$}.log' 'readiness logs must be scoped to this script run'
require 'CLEANED_UP=1' 'cleanup must be idempotent'
forbid "pkill -f" 'cleanup must never kill unrelated processes by name'
forbid 'if [ ! -f "${WEB_DIST}/index.html" ]' 'startup must not reuse a potentially stale Web bundle'

PORT_GUARD_LINE="$(grep -nF 'lsof -nP -iTCP:"${LISTEN_PORT}" -sTCP:LISTEN' "${DEV}" | head -1 | cut -d: -f1)"
WEB_BUILD_LINE="$(grep -nF '(cd web && bun run build)' "${DEV}" | head -1 | cut -d: -f1)"
if [ -z "${PORT_GUARD_LINE}" ] || [ -z "${WEB_BUILD_LINE}" ] || [ "${PORT_GUARD_LINE}" -ge "${WEB_BUILD_LINE}" ]; then
    echo 'FAIL: occupied-port guard must run before the Web build' >&2
    exit 1
fi

echo 'dev-contract: PASS'

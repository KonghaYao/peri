#!/bin/bash
# acp-hub 开发启动：同时拉起 acp-hub-server + acp-instance，Ctrl+C 一起退出。
#
# 流程：
#   0. 检查前端构建产物（web/dist，vite 产物不提交 git；缺失时自动构建）
#   1. 启动 server（cargo run，日志 .tmp/server.log）
#   2. 等待 server bootstrap 生成 instance token（~/.config/acp-hub/tokens.toml）
#   3. 提取 token → 写入 ~/.local/share/acp-hub/instance.token（0600）
#   4. 启动 instance（cargo run -- --token-file ...，日志 .tmp/instance.log）
#   5. 前台滚动两个日志；退出时清理两个进程（含 cargo 遗留子进程）
#
# 注意：所有变量引用一律用 ${VAR} 花括号 —— macOS 自带 bash 3.2 对
# `$VAR` 后紧跟全角字符（如 `）`）的解析有 bug，会吞掉变量名。
set -euo pipefail
cd "$(dirname "$0")"

# ---- 0. 前端构建产物检查（web/dist 缺失时先构建，否则 cargo build 失败） ----
WEB_DIST="$(pwd)/web/dist"
if [ ! -f "${WEB_DIST}/index.html" ]; then
    echo "==> 前端构建产物缺失（${WEB_DIST}），先构建 web/ ..."
    (cd web && bun install && bun run build)
fi

# ---- 路径（与 server 默认 XDG 语义一致，环境变量可覆盖） ----
CONFIG_DIR="${ACP_HUB_CONFIG_DIR:-$HOME/.config/acp-hub}"
DATA_DIR="${ACP_HUB_DATA_DIR:-$HOME/.local/share/acp-hub}"
TOKENS_FILE="${CONFIG_DIR}/tokens.toml"
INSTANCE_TOKEN_FILE="${DATA_DIR}/instance.token"
INSTANCE_DATA_DIR="${ACP_HUB_INSTANCE_DATA_DIR:-${DATA_DIR}/instance}"
LOG_DIR="$(pwd)/.tmp"
SERVER_LOG="${LOG_DIR}/server.log"
INSTANCE_LOG="${LOG_DIR}/instance.log"

mkdir -p "${LOG_DIR}" "${DATA_DIR}" "${INSTANCE_DATA_DIR}"

SERVER_PID=""
INSTANCE_PID=""
TAIL_PID=""

cleanup() {
    echo
    echo "==> 停止 instance / server ..."
    [ -n "${INSTANCE_PID}" ] && kill "${INSTANCE_PID}" 2>/dev/null || true
    [ -n "${SERVER_PID}" ] && kill "${SERVER_PID}" 2>/dev/null || true
    [ -n "${TAIL_PID}" ] && kill "${TAIL_PID}" 2>/dev/null || true
    # cargo run 被杀后其子进程可能存活（孤儿），按二进制路径补杀
    pkill -f 'target/debug/acp-instance' 2>/dev/null || true
    pkill -f 'target/debug/acp-hub-server' 2>/dev/null || true
}
trap cleanup EXIT INT TERM

# ---- 1. 启动 server ----
echo "==> 启动 acp-hub-server（日志: ${SERVER_LOG}）"
cargo run -q -p acp-hub-server >"${SERVER_LOG}" 2>&1 &
SERVER_PID=$!

# ---- 2. 等待 instance token 就绪（bootstrap 首次启动生成，原子写） ----
echo "==> 等待 server 初始化 instance token ..."
for _ in $(seq 1 60); do
    if [ -f "${TOKENS_FILE}" ] && grep -q 'role = "instance"' "${TOKENS_FILE}" 2>/dev/null; then
        break
    fi
    if ! kill -0 "${SERVER_PID}" 2>/dev/null; then
        echo "!! server 启动失败，日志如下："
        tail -30 "${SERVER_LOG}"
        exit 1
    fi
    sleep 1
done

if ! [ -f "${TOKENS_FILE}" ] || ! grep -q 'role = "instance"' "${TOKENS_FILE}" 2>/dev/null; then
    echo "!! 等待超时：tokens.toml 中未出现 instance token（${TOKENS_FILE}）"
    tail -30 "${SERVER_LOG}"
    exit 1
fi

# ---- 3. 提取未吊销 instance token（BSD awk 兼容，按空行分段） ----
TOKEN="$(awk '
    BEGIN { RS = "" }
    /role = "instance"/ && !/revoked = true/ {
        line = $0
        sub(/^.*token = "/, "", line)
        sub(/".*$/, "", line)
        print line
        exit
    }
' "${TOKENS_FILE}")"
if [ -z "${TOKEN}" ]; then
    echo "!! 未找到未吊销的 instance token（${TOKENS_FILE}）"
    exit 1
fi

umask 177
echo "${TOKEN}" > "${INSTANCE_TOKEN_FILE}"
umask 022
echo "==> instance token 就绪: ${INSTANCE_TOKEN_FILE}"

# ---- 4. 启动 instance ----
echo "==> 启动 acp-instance（日志: ${INSTANCE_LOG}）"
cargo run -q -p acp-instance --bin acp-instance -- \
    --token-file "${INSTANCE_TOKEN_FILE}" \
    --data-dir "${INSTANCE_DATA_DIR}" >"${INSTANCE_LOG}" 2>&1 &
INSTANCE_PID=$!

# ---- 5. 前台滚动日志，Ctrl+C 退出 ----
echo
echo "==> 全部就绪。server: ws://127.0.0.1:8456（默认）；instance 已连接。Ctrl+C 停止。"
echo
# tail 放后台 + wait：信号先到 bash（执行 cleanup 并 kill tail），wait 随 tail 退出，
# 避免「kill dev.sh 但 tail 在前台阻塞、trap 不执行」的退出卡死
tail -f "${SERVER_LOG}" "${INSTANCE_LOG}" &
TAIL_PID=$!
wait "${TAIL_PID}"

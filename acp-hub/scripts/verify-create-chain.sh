#!/bin/bash
# 链路 2 验收：chat/create 全链路（server → instance → ACP 进程 → committed ack）
#
# 场景（独立 temp 数据目录 + 随机端口，退出清理全部子进程）：
#   a. create 缺省 instanceId → 预期 error（复现 E3：UnknownInstance("local")）
#   b. create 显式 instanceId + PATH 无 peri → spawn_ack(false)（"agent spawn failed"）
#   c. create 显式 instanceId + fake peri(test-child) → committed + chatId（全链路）
#
# 用法: bash scripts/verify-create-chain.sh   （需先 cargo build --workspace）
set -u
cd "$(dirname "$0")/.."

T=$(mktemp -d /tmp/acp-hub-verify.XXXX)
PORT=$((18000 + RANDOM % 2000))
export ACP_HUB_CONFIG_DIR="$T/config"
export ACP_HUB_DATA_DIR="$T/data"
SERVER_PID=""
INSTANCE_PID=""

cleanup() {
  [ -n "${INSTANCE_PID}" ] && kill -TERM "${INSTANCE_PID}" 2>/dev/null || true
  [ -n "${SERVER_PID}" ] && kill -TERM "${SERVER_PID}" 2>/dev/null || true
  sleep 1
  pkill -f 'target/debug/acp-instance' 2>/dev/null || true
  pkill -f 'target/debug/acp-hub-server' 2>/dev/null || true
}
trap cleanup EXIT

echo "### T=$T PORT=$PORT"

# token 子命令不注入 env（CliOverrides::default），用 --config 文件重定向
cat > "$T/token-config.toml" <<INNER
config_dir = "$T/config"
data_dir = "$T/data"
INNER
CT=$(target/debug/acp-hub-server --config "$T/token-config.toml" token generate \
  --name verify-client --role full 2>/dev/null | tail -1)
[ -n "$CT" ] || { echo "!! no client token"; exit 1; }

target/debug/acp-hub-server run --listen-port "$PORT" > "$T/server.log" 2>&1 &
SERVER_PID=$!
sleep 2
for _ in $(seq 1 40); do
  grep -q 'role = "instance"' "$T/config/tokens.toml" 2>/dev/null && break
  sleep 0.5
done
IT=$(awk 'BEGIN { RS = "" } /role = "instance"/ && !/revoked = true/ {
  line = $0; sub(/^.*token = "/, "", line); sub(/".*$/, "", line); print line; exit }' \
  "$T/config/tokens.toml")
[ -n "$IT" ] || { echo "!! no instance token"; exit 1; }
echo "$IT" > "$T/instance.token"

start_instance() { # $1=logfile $2..=extra env
  local log="$1"; shift
  env "$@" target/debug/acp-instance --server-url "ws://127.0.0.1:${PORT}/instance" \
    --token-file "$T/instance.token" --data-dir "$T/idata" > "$log" 2>&1 &
  INSTANCE_PID=$!
  for _ in $(seq 1 20); do
    grep -q 'instance hello registered' "$T/server.log" && break
    sleep 0.5
  done
  sleep 1
}
stop_instance() {
  kill -TERM "${INSTANCE_PID}" 2>/dev/null || true
  sleep 1
  INSTANCE_PID=""
}
run_client() { # $1=label $2=instanceId-or-empty
  echo "== $1 =="
  node scripts/ws-verify-client.mjs "$CT" "ws://127.0.0.1:${PORT}/" "${2:-}" || true
}

start_instance "$T/instance-a.log"
run_client "a 缺省 instanceId（预期 error）" ""
stop_instance

start_instance "$T/instance-b.log" "PATH=/usr/bin:/bin:/usr/sbin:/sbin"
run_client "b 显式 instanceId + 无 peri PATH（预期 error）" "bootstrap-instance"
stop_instance

# 重启 server（b 场景强杀 instance 触发 session_gap degraded 竞态，§17.2 缺陷，
# 会挡住后续 create；c 用独立 server 实例验证全链路）
kill -TERM "${SERVER_PID}" 2>/dev/null || true
sleep 2
target/debug/acp-hub-server run --listen-port "$PORT" > "$T/server2.log" 2>&1 &
SERVER_PID=$!
sleep 2

mkdir -p "$T/fakebin"
printf '#!/bin/sh\nexec %s/target/debug/test-child "$@"\n' "$(pwd)" > "$T/fakebin/peri"
chmod +x "$T/fakebin/peri"
start_instance "$T/instance-c.log" "PATH=$T/fakebin:/usr/bin:/bin"
run_client "c 显式 instanceId + test-child（预期 committed）" "bootstrap-instance"
stop_instance

echo
echo "=== instance-a.log（无 spawn 痕迹 = F1 在 server 本地） ==="
sed 's/\x1b\[[0-9;]*m//g' "$T/instance-a.log" | grep -v "^$"
echo "=== instance-b.log（spawn 失败） ==="
sed 's/\x1b\[[0-9;]*m//g' "$T/instance-b.log" | grep -v "^$"
echo "=== instance-c.log（进程启动） ==="
sed 's/\x1b\[[0-9;]*m//g' "$T/instance-c.log" | grep -v "^$"
echo "=== server.log 关键行 ==="
grep -aE "command.submit|command.error|command.committed|UnknownInstance|spawn" "$T/server.log" \
  | sed 's/\x1b\[[0-9;]*m//g' | tail -20

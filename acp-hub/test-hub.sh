#!/bin/bash
set -e

cd "$(dirname "$0")/.."

# 编译
cargo build -q -p acp-hub --bin acp-hub --bin test-child
TEST_CHILD="$(pwd)/target/debug/test-child"

# 启动 acp-hub，默认用 test-child 作为子进程
# 用法:
#   bash acp-hub/test-hub.sh                     → 默认 test-child
#   bash acp-hub/test-hub.sh --pretty            → 人类可读日志
#   echo '{"jsonrpc":...}' | bash acp-hub/test-hub.sh | jq   → 管道测试
exec cargo run -q -p acp-hub --bin acp-hub -- "$@" -- "$TEST_CHILD"

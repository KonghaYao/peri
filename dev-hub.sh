#!/bin/bash
set -e

cd "$(dirname "$0")"

# 加载 .env
set -a; source .env; set +a

# 确保日志目录存在
mkdir -p "$(dirname "$RUST_LOG_FILE")"

# 编译 test-child（默认子进程）
cargo build -q -p acp-hub --bin test-child

# 如果用户没指定 -- <child-command>，默认用 test-child
HAS_DASHDASH=false
for a in "$@"; do [[ "$a" == "--" ]] && HAS_DASHDASH=true; done
if ! $HAS_DASHDASH; then
    exec cargo run -p acp-hub --bin acp-hub -- "$@" -- ./target/debug/test-child
else
    exec cargo run -p acp-hub --bin acp-hub -- "$@"
fi
